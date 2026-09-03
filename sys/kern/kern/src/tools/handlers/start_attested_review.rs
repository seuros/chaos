use std::collections::BTreeMap;

use chaos_parrot::sanitize::JsonSchema;
use chaos_parrot::sanitize::ResponsesApiTool;
use serde::Deserialize;

use crate::client_common::tools::ToolSpec;
use crate::function_tool::FunctionCallError;
use crate::reviewer_orchestration::REVIEW_VERDICT_TOOL;
use crate::reviewer_orchestration::ReviewerSelection;
use crate::reviewer_orchestration::SessionReviewerBoundary;
use crate::reviewer_orchestration::build_reviewer_prompt;
use crate::reviewer_orchestration::progress_json;
use crate::reviewer_orchestration::resolve_reviewer_binding;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::handlers::extract_function_arguments;
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;

pub(crate) const TOOL_NAME: &str = "start_attested_review";

#[derive(Debug, Deserialize)]
struct StartAttestedReviewArgs {
    instructions: String,
    model_provider: String,
    model: String,
    server: String,
    idempotency_key: String,
}

pub struct StartAttestedReviewHandler;

impl ToolHandler for StartAttestedReviewHandler {
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
        let args: StartAttestedReviewArgs = parse_arguments(&arguments)?;
        let server = args.server.trim();
        if server.is_empty() {
            return Err(model_error("review service server name cannot be empty"));
        }
        let review_sink_available = invocation
            .session
            .services
            .mcp_registry
            .current_manager()
            .list_all_tools()
            .await
            .values()
            .any(|tool| tool.server_name == server && tool.tool_name == REVIEW_VERDICT_TOOL);
        if !review_sink_available {
            return Err(model_error(&format!(
                "MCP server `{server}` does not expose `{REVIEW_VERDICT_TOOL}` for the current session"
            )));
        }
        let idempotency_key = args.idempotency_key.trim();
        if idempotency_key.is_empty() {
            return Err(model_error("review idempotency key cannot be empty"));
        }

        let binding = resolve_reviewer_binding(
            invocation.session.as_ref(),
            invocation.turn.as_ref(),
            &args.model_provider,
            &args.model,
        )
        .await
        .map_err(anyhow_model_error)?;
        let prompt = build_reviewer_prompt(&args.instructions).map_err(anyhow_model_error)?;
        let owner_process_id = invocation.session.conversation_id.to_string();
        let orchestrator = SessionReviewerBoundary::orchestrator(
            invocation.session.clone(),
            invocation.turn.clone(),
        )
        .map_err(anyhow_model_error)?;
        let run = orchestrator
            .start_run(
                &owner_process_id,
                vec![ReviewerSelection {
                    binding,
                    prompt,
                    mcp_server: server.to_string(),
                    mcp_tool: REVIEW_VERDICT_TOOL.to_string(),
                    idempotency_key: idempotency_key.to_string(),
                }],
            )
            .await
            .map_err(anyhow_model_error)?;
        let attempts = orchestrator
            .resume_run(&owner_process_id, &run.id)
            .await
            .map_err(anyhow_model_error)?;
        let output = progress_json(&run.id, &attempts);
        Ok(FunctionToolOutput::from_text(
            output.to_string(),
            Some(true),
        ))
    }
}

pub(crate) fn tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "instructions".to_string(),
            JsonSchema::String {
                description: Some(
                    "Blind review artifact and exact acceptance criteria for the reviewer. ChaOS appends the strict review-output contract."
                        .to_string(),
                ),
            },
        ),
        (
            "model_provider".to_string(),
            JsonSchema::String {
                description: Some(
                    "Configured provider/account id. ChaOS resolves its credential identity without exposing it."
                        .to_string(),
                ),
            },
        ),
        (
            "model".to_string(),
            JsonSchema::String {
                description: Some(
                    "Exact cached model slug. Its canonical family must be known.".to_string(),
                ),
            },
        ),
        (
            "server".to_string(),
            JsonSchema::String {
                description: Some(
                    "Configured MCP server that currently exposes `submit_review_verdict`."
                        .to_string(),
                ),
            },
        ),
        (
            "idempotency_key".to_string(),
            JsonSchema::String {
                description: Some(
                    "Stable key allocated by the review service for this exact verdict and any retry."
                        .to_string(),
                ),
            },
        ),
    ]);
    ToolSpec::Function(ResponsesApiTool {
        name: TOOL_NAME.to_string(),
        description: "Start one host-attested independent review through a selected MCP server's currently visible `submit_review_verdict` capability. ChaOS binds an exact configured provider/account and canonical model family, runs the reviewer with strict structured output, persists the state machine before side effects, and submits the verdict with protected provenance. Use only after the user authorized independent or multi-model review. Returns a run id immediately; use resume_attested_review until terminal."
            .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec![
                "instructions".to_string(),
                "model_provider".to_string(),
                "model".to_string(),
                "server".to_string(),
                "idempotency_key".to_string(),
            ]),
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
