//! Fixed host-only root controls, dispatched through normal kernel policy.

use super::*;
use crate::tools::parallel::ToolCallRuntime;
use crate::tools::router::{ToolCallSource, ToolRouter};
use chaos_ipc::models::{ResponseInputItem, ResponseItem};

pub(super) enum RootControl {
    EnableTools,
    DisableTools,
    Synopsis,
}

impl RootControl {
    fn name(&self) -> &'static str {
        match self {
            Self::EnableTools => "enable_tools",
            Self::DisableTools => "disable_tools",
            Self::Synopsis => "run_synopsis",
        }
    }
}

impl WorkflowScope {
    /// Change this external owner's groups through the native control handler.
    /// This trusted top-level API never accepts a caller-selected tool name.
    pub async fn configure_groups(
        &self,
        ctx: ExecutionContext<'_>,
        enabled: bool,
    ) -> Result<ToolOutput, ToolError> {
        let control = if enabled {
            RootControl::EnableTools
        } else {
            RootControl::DisableTools
        };
        self.dispatch_root(ctx, control).await
    }

    pub(super) async fn dispatch_root(
        &self,
        ctx: ExecutionContext<'_>,
        control: RootControl,
    ) -> Result<ToolOutput, ToolError> {
        self.check_owner(&ctx)?;
        let name = control.name();
        let owner = self.0.owner.as_ref().ok_or_else(|| {
            ToolError::Execution(format!("{name} requires a session-owned operation process"))
        })?;
        owner.check(&ctx)?;
        let original = &self.0.invocation;
        let turn = original
            .session
            .try_effective_turn_context(&original.turn)
            .await
            .map_err(execution)?;
        let router = crate::chaos::built_tools(&original.session, &turn, &[], &self.0.lifetime)
            .await
            .map_err(execution)?;
        // Direct is available only to these fixed trusted root APIs, never
        // caller JSON or a tree leaf. No group is implicitly activated.
        let call = ToolRouter::build_tool_call(
            &original.session,
            ResponseItem::FunctionCall {
                id: None,
                name: name.into(),
                namespace: None,
                arguments: ctx.params.to_string(),
                call_id: original.call_id.clone(),
                provider_metadata: None,
            },
        )
        .await
        .map_err(execution)?
        .ok_or_else(|| ToolError::Execution(format!("{name} tool unavailable")))?;
        let result = ToolCallRuntime::new(
            router,
            original.session.clone(),
            turn,
            original.tracker.clone(),
        )
        .handle_tool_call_with_source(call, ToolCallSource::Direct, self.0.lifetime.clone())
        .await
        .map_err(execution)?;
        if !result.result.success_for_logging() {
            return Err(ToolError::Execution(result.result.log_preview()));
        }
        let ResponseInputItem::FunctionCallOutput { output, .. } = result.into_response() else {
            return Err(ToolError::Internal(format!("unexpected {name} output")));
        };
        let text = output
            .body
            .to_text()
            .ok_or_else(|| ToolError::Internal(format!("missing {name} output")))?;
        let value: serde_json::Value = serde_json::from_str(&text).map_err(execution)?;
        ToolOutput::structured(value).map_err(execution)
    }
}

fn execution(error: impl std::fmt::Display) -> ToolError {
    ToolError::Execution(error.to_string())
}
