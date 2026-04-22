//! Shared frontend turn-building helpers.

use std::path::PathBuf;

use chaos_ipc::config_types::CollaborationMode;
use chaos_ipc::config_types::Personality;
use chaos_ipc::config_types::ReasoningSummary;
use chaos_ipc::config_types::ServiceTier;
use chaos_ipc::openai_models::ReasoningEffort;
use chaos_ipc::protocol::ApprovalPolicy;
use chaos_ipc::protocol::Op;
use chaos_ipc::protocol::SandboxPolicy;
use chaos_ipc::user_input::TextElement;
use chaos_ipc::user_input::UserInput;
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct TurnContext {
    pub cwd: PathBuf,
    pub approval_policy: ApprovalPolicy,
    pub sandbox_policy: SandboxPolicy,
    pub model: String,
    pub effort: Option<ReasoningEffort>,
    pub summary: Option<ReasoningSummary>,
    pub service_tier: Option<Option<ServiceTier>>,
    pub final_output_json_schema: Option<Value>,
    pub collaboration_mode: Option<CollaborationMode>,
    pub personality: Option<Personality>,
}

#[derive(Debug, Clone)]
pub struct TurnSubmission {
    pub items: Vec<UserInput>,
    pub context: TurnContext,
}

impl TurnSubmission {
    pub fn new(items: Vec<UserInput>, context: TurnContext) -> Self {
        Self { items, context }
    }

    pub fn text(text: String, text_elements: Vec<TextElement>, context: TurnContext) -> Self {
        Self {
            items: vec![UserInput::Text {
                text,
                text_elements,
            }],
            context,
        }
    }

    pub fn into_op(self) -> Op {
        let ctx = self.context;
        Op::UserTurn {
            items: self.items,
            cwd: ctx.cwd,
            approval_policy: ctx.approval_policy,
            sandbox_policy: ctx.sandbox_policy,
            model: ctx.model,
            effort: ctx.effort,
            summary: ctx.summary,
            service_tier: ctx.service_tier,
            final_output_json_schema: ctx.final_output_json_schema,
            collaboration_mode: ctx.collaboration_mode,
            personality: ctx.personality,
        }
    }
}
