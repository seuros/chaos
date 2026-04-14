//! Root of the `chaos-kern` library.

// Prevent accidental direct writes to stdout/stderr in library code. All
// user-visible output must go through the appropriate abstraction (e.g.,
// the TUI or the tracing stack).
#![deny(clippy::print_stdout, clippy::print_stderr)]

// Force-link catalog module crates so the linker preserves their
// `inventory::submit!` registrations. Without these references the linker
// may drop the object files and `inventory::iter` will not discover the
// static CatalogRegistration entries.
extern crate chaos_arsenal;
extern crate chaos_cron;
extern crate chaos_git;

pub mod api_bridge;
mod apply_patch;
mod arc_monitor;
pub mod auth;
pub mod builtin_mcp_resources;
pub(crate) mod catalog;
pub mod chaos;
mod clamp_bridge;
mod client;
mod client_common;
pub use chaos::SteerInputError;
mod compact_remote;
mod process;
pub use process::Process;
pub use process::ProcessConfigSnapshot;
mod chaos_delegate;
mod command_canonicalization;
pub mod config;
pub mod config_loader;
mod context_manager;
mod contextual_user_message;
pub mod custom_prompts;
pub mod env;
mod environment_context;
pub mod error;
pub mod exec;
pub mod exec_env;
mod exec_policy;
pub mod external_agent_config;
pub mod features;
mod file_watcher;
mod flags;
pub mod git_info;

pub mod instructions;
pub mod landlock;
pub mod mcp;
mod mcp_manage_tools;
mod mcp_tool_approval_templates;
mod minions;
pub mod models_manager;
mod network_policy_decision;
pub mod network_proxy_loader;
mod original_image_detail;
pub use chaos_mcp_runtime::manager::MCP_SANDBOX_STATE_LOGGER;
pub use chaos_mcp_runtime::manager::SandboxState;
pub use text_encoding::bytes_to_string_smart;
mod mcp_tool_call;
pub mod mention_syntax;
mod mentions;
mod message_history;
mod model_provider_info;
pub mod path_utils;
pub mod personality_migration;
mod sandbox_tags;
pub mod sandboxing;
mod session_prefix;
mod stream_events_utils;
pub mod test_support;
mod text_encoding;
pub mod token_data;
mod traits_impl;
mod truncate;
mod unified_exec;
pub use client::X_RESPONSESAPI_INCLUDE_TIMING_METRICS_HEADER;
pub use model_provider_info::ModelProviderInfo;
pub use model_provider_info::OPENAI_DEFAULT_BASE_URL;
pub use model_provider_info::OPENAI_PROVIDER_ID;
pub use model_provider_info::WireApi;
pub use model_provider_info::built_in_model_providers;
pub use model_provider_info::create_oss_provider_with_base_url;
mod event_mapping;
mod process_table;
mod response_debug_context;
pub mod review_format;
pub mod review_prompts;
pub mod web_search;
pub use process_table::NewProcess;
pub use process_table::ProcessTable;
// Re-export common auth types for workspace consumers
pub use auth::AuthManager;
pub use auth::ChaosAuth;
pub mod default_client;
pub mod project_doc;
mod rollout;
pub mod runtime_db;
pub(crate) mod safety;
pub mod shell;
pub mod shell_snapshot;
pub mod spawn;
pub mod terminal;
mod tools;
pub mod turn_diff_tracker;
mod turn_metadata;
mod turn_timing;
pub use rollout::INTERACTIVE_SESSION_SOURCES;
pub use rollout::ProcessItem;
pub use rollout::ProcessSortKey;
pub use rollout::ProcessesPage;
pub use rollout::RolloutRecorder;
pub use rollout::RolloutRecorderParams;
pub use rollout::SessionMeta;
pub use rollout::append_process_name;
pub use rollout::find_process_id_by_name;
pub use rollout::find_process_name_by_id;
pub use rollout::find_process_names_by_ids;
pub use rollout::list::Cursor;
pub use rollout::list::parse_cursor;
pub use rollout::policy::EventPersistenceMode;
mod function_tool;
mod state;
mod tasks;
mod user_shell_command;
pub mod util;
pub(crate) use chaos_ipc::protocol;
pub(crate) use chaos_sh::bash;
pub(crate) use chaos_sh::is_dangerous_command;
pub(crate) use chaos_sh::is_safe_command;
pub(crate) use chaos_sh::parse_command;

pub use client::ModelClient;
pub use client::ModelClientSession;
pub use client::X_CHAOS_TURN_METADATA_HEADER;
pub use client_common::Prompt;
pub use client_common::REVIEW_PROMPT;
pub use client_common::ResponseEvent;
pub use client_common::ResponseStream;
pub use compact::content_items_to_text;
pub use event_mapping::parse_turn_item;
pub use exec_policy::ExecPolicyError;
pub use exec_policy::check_execpolicy_for_warnings;
pub use exec_policy::format_exec_policy_error_with_source;
pub use exec_policy::load_exec_policy;
pub use file_watcher::FileWatcherEvent;
pub use mcp_manage_tools::McpAddServerParams;
pub use mcp_manage_tools::add_server_to_dot_mcp_json;
pub use mcp_manage_tools::build_project_mcp_refresh_config;
pub use mcp_manage_tools::project_mcp_json_path_for_cwd;
pub use safety::get_platform_sandbox;
pub use turn_metadata::build_turn_metadata_header;
pub mod compact;
pub mod otel_init;
