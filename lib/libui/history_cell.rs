//! Transcript/history cells for the Chaos TUI.
//!
//! A `HistoryCell` is the unit of display in the conversation UI, representing both committed
//! transcript entries and, transiently, an in-flight active cell that can mutate in place while
//! streaming.
//!
//! The transcript overlay (`Ctrl+T`) appends a cached live tail derived from the active cell, and
//! that cached tail is refreshed based on an active-cell cache key. Cells that change based on
//! elapsed time expose `transcript_animation_tick()`, and code that mutates the active cell in place
//! bumps the active-cell revision tracked by `ChatWidget`, so the cache key changes whenever the
//! rendered transcript output can change.

mod diff;
mod render;
mod state;

pub use diff::PatchHistoryCell;
pub use diff::new_patch_apply_failure;
pub use diff::new_patch_event;
pub use diff::new_view_image_tool_call;

pub use render::runtime_metrics_label;
pub use render::with_border_with_inner_width;

pub use state::AgentMessageCell;
pub use state::ApprovalDecisionActor;
pub use state::CompositeHistoryCell;
pub use state::DeprecationNoticeCell;
pub use state::FinalMessageSeparator;
pub use state::HistoryCell;
pub use state::McpToolCallCell;
pub use state::PlainHistoryCell;
pub use state::PlanUpdateCell;
pub use state::PrefixedWrappedHistoryCell;
pub use state::ProposedPlanCell;
pub use state::ProposedPlanStreamCell;
pub use state::ReasoningSummaryCell;
pub use state::RequestUserInputResultCell;
pub use state::SessionInfoCell;
pub use state::UnifiedExecInteractionCell;
pub use state::UnifiedExecProcessDetails;
pub use state::UserHistoryCell;
pub use state::WebSearchCell;
pub use state::empty_mcp_output;
pub use state::new_active_mcp_tool_call;
pub use state::new_active_web_search_call;
pub use state::new_approval_decision_cell;
pub use state::new_deprecation_notice;
pub use state::new_error_event;
pub use state::new_image_generation_call;
pub use state::new_info_event;
pub use state::new_mcp_tools_output;
pub use state::new_plan_update;
pub use state::new_proposed_plan;
pub use state::new_proposed_plan_stream;
pub use state::new_reasoning_summary_block;
pub use state::new_review_status_line;
pub use state::new_session_info;
pub use state::new_unified_exec_interaction;
pub use state::new_unified_exec_processes_output;
pub use state::new_user_prompt;
pub use state::new_warning_event;
pub use state::new_web_search_call;

#[cfg(test)]
pub(crate) mod tests;
