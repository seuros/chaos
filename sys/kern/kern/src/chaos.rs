//! Chaos session management — thin aggregator module.
//!
//! All implementation lives in the submodules below. This file only
//! declares the module tree and re-exports the public surface that the
//! rest of the crate and external callers depend on.

// ── Existing submodules (untouched) ─────────────────────────────────────────
pub(super) mod approvals;
pub(super) mod mcp_integration;
pub(super) mod response_parsing;
mod rollout_reconstruction;
#[cfg(test)]
#[path = "chaos/rollout_reconstruction_tests.rs"]
mod rollout_reconstruction_tests;
pub(super) mod settings;

// ── New submodules ───────────────────────────────────────────────────────────
pub(crate) mod session;
pub(crate) mod spawn;
pub(crate) mod submission_loop;
pub(crate) mod turn;
pub(crate) mod turn_context;

// ── Re-exports for the rest of the crate ────────────────────────────────────

// Chaos engine + spawn helpers
pub(crate) use spawn::Chaos;
pub(crate) use spawn::ChaosSpawnArgs;
pub(crate) use spawn::ChaosSpawnOk;
pub(crate) use spawn::INITIAL_SUBMIT_ID;
pub(crate) use spawn::SUBMISSION_CHANNEL_CAPACITY;

// Session
pub(crate) use session::Session;

// Turn context / configuration types
pub(crate) use turn_context::PreviousTurnSettings;
pub(crate) use turn_context::SessionConfiguration;
pub(crate) use turn_context::SessionSettingsUpdate;
pub(crate) use turn_context::TurnContext;

// Turn execution helpers used by compact / clamp_bridge / tasks
pub(crate) use turn::built_tools;
pub(crate) use turn::get_last_assistant_message_from_turn;
pub(crate) use turn::run_turn;

// Steer-input error — part of the public chaos API, referenced by
// process.rs and chaos_delegate.rs.
use chaos_ipc::user_input::UserInput;

#[derive(Debug, PartialEq)]
pub enum SteerInputError {
    NoActiveTurn(Vec<UserInput>),
    ExpectedTurnMismatch { expected: String, actual: String },
    EmptyInput,
}

// ── Test-only surface ────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "chaos_tests.rs"]
mod tests;

// Test helpers used by test files outside this module
#[cfg(test)]
pub(crate) use spawn::completed_session_loop_termination;
#[cfg(test)]
pub(crate) use spawn::session_loop_termination_from_handle;
#[cfg(test)]
pub(crate) use submission_loop::initial_replay_event_msgs;
#[cfg(test)]
pub(crate) use tests::make_session_and_context;
#[cfg(test)]
pub(crate) use tests::make_session_and_context_with_rx;
#[cfg(test)]
pub(crate) use tests::make_session_configuration_for_tests;

// Types and items needed by chaos_tests/* via `use super::*`. These are
// gated behind cfg(test) so they do not bloat the production surface.
#[cfg(test)]
pub(crate) use crate::AuthManager;
#[cfg(test)]
pub(crate) use crate::client::ModelClient;
#[cfg(test)]
pub(crate) use crate::collaboration_modes::CollaborationModesConfig;
#[cfg(test)]
pub(crate) use crate::compact;
#[cfg(test)]
pub(crate) use crate::compact::collect_user_messages;
#[cfg(test)]
pub(crate) use crate::config::Config;
#[cfg(test)]
pub(crate) use crate::context_manager::ContextManager;
#[cfg(test)]
pub(crate) use crate::exec::StreamOutput;
#[cfg(test)]
pub(crate) use crate::exec_policy::ExecPolicyManager;
#[cfg(test)]
pub(crate) use crate::file_watcher::FileWatcher;
#[cfg(test)]
pub(crate) use crate::mcp::McpManager;
#[cfg(test)]
pub(crate) use crate::minions::AgentControl;
#[cfg(test)]
pub(crate) use crate::minions::AgentStatus;
#[cfg(test)]
pub(crate) use crate::models_manager::manager::ModelsManager;
#[cfg(test)]
pub(crate) use crate::state::ActiveTurn;
#[cfg(test)]
pub(crate) use crate::state::SessionServices;
#[cfg(test)]
pub(crate) use crate::state::SessionState;
#[cfg(test)]
pub(crate) use crate::stream_events_utils::HandleOutputCtx;
#[cfg(test)]
pub(crate) use crate::stream_events_utils::handle_output_item_done;
#[cfg(test)]
pub(crate) use crate::tasks::ReviewTask;
#[cfg(test)]
pub(crate) use crate::tools::network_approval::NetworkApprovalService;
#[cfg(test)]
pub(crate) use crate::tools::parallel::ToolCallRuntime;
#[cfg(test)]
pub(crate) use crate::tools::sandboxing::ApprovalStore;
#[cfg(test)]
pub(crate) use crate::unified_exec::UnifiedExecProcessManager;
#[cfg(test)]
pub(crate) use chaos_dtrace::Hooks;
#[cfg(test)]
pub(crate) use chaos_dtrace::HooksConfig;
#[cfg(test)]
pub(crate) use chaos_ipc::config_types::CollaborationMode;
#[cfg(test)]
pub(crate) use chaos_ipc::config_types::ModeKind;
#[cfg(test)]
pub(crate) use chaos_ipc::config_types::Settings;
#[cfg(test)]
pub(crate) use chaos_ipc::dynamic_tools::DynamicToolSpec;
#[cfg(test)]
pub(crate) use chaos_ipc::items::TurnItem;
#[cfg(test)]
pub(crate) use chaos_ipc::items::UserMessageItem;
#[cfg(test)]
pub(crate) use chaos_ipc::openai_models::ModelInfo;
#[cfg(test)]
pub(crate) use chaos_ipc::openai_models::ReasoningEffort as ReasoningEffortConfig;
#[cfg(test)]
pub(crate) use chaos_ipc::permissions::SocketPolicy;
#[cfg(test)]
pub(crate) use chaos_ipc::protocol::ChaosErrorInfo;
#[cfg(test)]
pub(crate) use chaos_ipc::protocol::ErrorEvent;
#[cfg(test)]
pub(crate) use chaos_ipc::protocol::Event;
#[cfg(test)]
pub(crate) use chaos_ipc::protocol::EventMsg;
#[cfg(test)]
pub(crate) use chaos_ipc::protocol::ItemCompletedEvent;
#[cfg(test)]
pub(crate) use chaos_ipc::protocol::ItemStartedEvent;
#[cfg(test)]
pub(crate) use chaos_ipc::protocol::Op;
#[cfg(test)]
pub(crate) use chaos_ipc::protocol::RolloutItem;
#[cfg(test)]
pub(crate) use chaos_ipc::protocol::SessionSource;
#[cfg(test)]
pub(crate) use chaos_ipc::protocol::TurnAbortReason;
#[cfg(test)]
pub(crate) use chaos_ipc::protocol::TurnContextItem;
#[cfg(test)]
pub(crate) use chaos_syslog::SessionTelemetry;
#[cfg(test)]
pub(crate) use chaos_syslog::current_span_w3c_trace_context;
#[cfg(test)]
pub(crate) use chaos_syslog::set_parent_from_w3c_trace_context;
#[cfg(test)]
pub(crate) use response_parsing::AssistantMessageStreamParsers;
#[cfg(test)]
pub(crate) use std::sync::atomic::AtomicU64;
#[cfg(test)]
pub(crate) use submission_loop::handlers;
#[cfg(test)]
pub(crate) use submission_loop::submission_dispatch_span;
#[cfg(test)]
pub(crate) use tokio::sync::Mutex;
#[cfg(test)]
pub(crate) use tokio::sync::RwLock;
#[cfg(test)]
pub(crate) use tokio::sync::watch;
#[cfg(test)]
pub(crate) use tokio_util::sync::CancellationToken;
#[cfg(test)]
pub(crate) use tracing::Instrument;
#[cfg(test)]
pub(crate) use tracing::info_span;
