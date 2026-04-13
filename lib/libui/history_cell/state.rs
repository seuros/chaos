//! State structs and models for history cells.
//!
//! Contains the `HistoryCell` trait and every concrete cell type together with
//! their `HistoryCell` implementations and constructor helpers.

mod cells_basic;
mod cells_composite;
mod cells_mcp;
mod cells_misc;
mod cells_plan;
mod trait_def;

pub use cells_basic::AgentMessageCell;
pub use cells_basic::PlainHistoryCell;
pub use cells_basic::PrefixedWrappedHistoryCell;
pub use cells_basic::ReasoningSummaryCell;
pub use cells_basic::UnifiedExecInteractionCell;
pub use cells_basic::UserHistoryCell;
pub use cells_basic::new_reasoning_summary_block;
pub use cells_basic::new_unified_exec_interaction;
pub use cells_basic::new_user_prompt;

pub use cells_composite::ApprovalDecisionActor;
pub(super) use cells_composite::CompletedMcpToolCallWithImageOutput;
pub use cells_composite::CompositeHistoryCell;
pub use cells_composite::SessionInfoCell;
pub use cells_composite::UnifiedExecProcessDetails;
pub use cells_composite::new_session_info;
pub use cells_composite::new_unified_exec_processes_output;

pub use cells_misc::FinalMessageSeparator;
pub use cells_misc::new_approval_decision_cell;
pub use cells_misc::new_error_event;
pub use cells_misc::new_image_generation_call;
pub use cells_misc::new_info_event;
pub use cells_misc::new_review_status_line;
pub use cells_misc::new_warning_event;

pub use cells_mcp::DeprecationNoticeCell;
pub use cells_mcp::McpToolCallCell;
pub use cells_mcp::RequestUserInputResultCell;
pub use cells_mcp::WebSearchCell;
pub use cells_mcp::empty_mcp_output;
pub use cells_mcp::new_active_mcp_tool_call;
pub use cells_mcp::new_active_web_search_call;
pub use cells_mcp::new_deprecation_notice;
pub use cells_mcp::new_mcp_tools_output;
pub use cells_mcp::new_web_search_call;

pub use cells_plan::PlanUpdateCell;
pub use cells_plan::ProposedPlanCell;
pub use cells_plan::ProposedPlanStreamCell;
pub use cells_plan::new_plan_update;
pub use cells_plan::new_proposed_plan;
pub use cells_plan::new_proposed_plan_stream;

pub use trait_def::HistoryCell;
