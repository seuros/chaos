use std::collections::BTreeMap;

use chaos_parrot::sanitize::JsonSchema;
use chaos_parrot::sanitize::ResponsesApiTool;
use serde::Deserialize;
use serde_json::json;

use crate::client_common::tools::ToolSpec;
use crate::function_tool::FunctionCallError;
use crate::reviewer_orchestration::SessionReviewerBoundary;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::handlers::extract_function_arguments;
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;

pub(crate) const TOOL_NAME: &str = "cancel_attested_review";

#[derive(Debug, Deserialize)]
struct CancelAttestedReviewArgs {
    attempt_id: String,
    reason: String,
}

pub struct CancelAttestedReviewHandler;

impl ToolHandler for CancelAttestedReviewHandler {
    type Output = FunctionToolOutput;

    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    #[allow(clippy::manual_async_fn)]
    fn is_mutating(&self, _invocation: &ToolInvocation) -> impl Future<Output = bool> + Send + '_ {
        async { true }
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<Self::Output, FunctionCallError> {
        let arguments = extract_function_arguments(invocation.payload, TOOL_NAME)?;
        let args: CancelAttestedReviewArgs = parse_arguments(&arguments)?;
        let attempt_id = args.attempt_id.trim();
        let reason = args.reason.trim();
        if attempt_id.is_empty() {
            return Err(model_error("review attempt id cannot be empty"));
        }
        if reason.is_empty() {
            return Err(model_error("review cancellation reason cannot be empty"));
        }

        let owner_process_id = invocation.session.conversation_id.to_string();
        let orchestrator = SessionReviewerBoundary::orchestrator(
            invocation.session.clone(),
            invocation.turn.clone(),
        )
        .map_err(anyhow_model_error)?;
        let cancelled = orchestrator
            .cancel_attempt(&owner_process_id, attempt_id, reason)
            .await
            .map_err(anyhow_model_error)?;
        Ok(FunctionToolOutput::from_text(
            json!({
                "attempt_id": attempt_id,
                "cancelled": cancelled
            })
            .to_string(),
            Some(true),
        ))
    }
}

pub(crate) fn tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "attempt_id".to_string(),
            JsonSchema::String {
                description: Some(
                    "Attempt id returned by an attested review run owned by this ChaOS process."
                        .to_string(),
                ),
            },
        ),
        (
            "reason".to_string(),
            JsonSchema::String {
                description: Some(
                    "Non-empty durable reason for cancelling the reviewer attempt.".to_string(),
                ),
            },
        ),
    ]);
    ToolSpec::Function(ResponsesApiTool {
        name: TOOL_NAME.to_string(),
        description: "Cancel a nonterminal attested reviewer attempt owned by this ChaOS process. ChaOS records the cancellation before idempotently stopping the reviewer process."
            .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["attempt_id".to_string(), "reason".to_string()]),
            additional_properties: Some(false.into()),
        },
        output_schema: None,
    })
}

fn anyhow_model_error(error: anyhow::Error) -> FunctionCallError {
    FunctionCallError::RespondToModel(format!("{error:#}"))
}

fn model_error(message: &str) -> FunctionCallError {
    FunctionCallError::RespondToModel(message.to_string())
}
