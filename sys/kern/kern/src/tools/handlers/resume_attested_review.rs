use std::collections::BTreeMap;
use std::time::Duration;
use std::time::Instant;

use chaos_parrot::sanitize::JsonSchema;
use chaos_parrot::sanitize::ResponsesApiTool;
use serde::Deserialize;

use crate::client_common::tools::ToolSpec;
use crate::function_tool::FunctionCallError;
use crate::reviewer_orchestration::SessionReviewerBoundary;
use crate::reviewer_orchestration::progress_json;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::handlers::extract_function_arguments;
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;

pub(crate) const TOOL_NAME: &str = "resume_attested_review";
const DEFAULT_WAIT_MS: u64 = 30_000;
const MAX_WAIT_MS: u64 = 600_000;
const POLL_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Debug, Deserialize)]
struct ResumeAttestedReviewArgs {
    run_id: String,
    #[serde(default = "default_wait_ms")]
    wait_ms: u64,
}

const fn default_wait_ms() -> u64 {
    DEFAULT_WAIT_MS
}

pub struct ResumeAttestedReviewHandler;

impl ToolHandler for ResumeAttestedReviewHandler {
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
        let args: ResumeAttestedReviewArgs = parse_arguments(&arguments)?;
        let run_id = args.run_id.trim();
        if run_id.is_empty() {
            return Err(model_error("review run id cannot be empty"));
        }
        if args.wait_ms > MAX_WAIT_MS {
            return Err(model_error(&format!("wait_ms cannot exceed {MAX_WAIT_MS}")));
        }

        let owner_process_id = invocation.session.conversation_id.to_string();
        let orchestrator = SessionReviewerBoundary::orchestrator(
            invocation.session.clone(),
            invocation.turn.clone(),
        )
        .map_err(anyhow_model_error)?;
        let started = Instant::now();
        loop {
            let attempts = orchestrator
                .resume_run(&owner_process_id, run_id)
                .await
                .map_err(anyhow_model_error)?;
            if attempts.iter().all(|attempt| attempt.state.is_terminal())
                || started.elapsed() >= Duration::from_millis(args.wait_ms)
            {
                let output = progress_json(run_id, &attempts);
                return Ok(FunctionToolOutput::from_text(
                    output.to_string(),
                    Some(true),
                ));
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}

pub(crate) fn tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "run_id".to_string(),
            JsonSchema::String {
                description: Some(
                    "Run id returned by start_attested_review in this same ChaOS process."
                        .to_string(),
                ),
            },
        ),
        (
            "wait_ms".to_string(),
            JsonSchema::Integer {
                description: Some(format!(
                    "Maximum time to poll this invocation. Defaults to {DEFAULT_WAIT_MS} and is capped at {MAX_WAIT_MS}."
                )),
            },
        ),
    ]);
    ToolSpec::Function(ResponsesApiTool {
        name: TOOL_NAME.to_string(),
        description: "Resume an owner-fenced, persisted attested review state machine. It never respawns an acknowledged attempt and retries an acknowledgement-unknown submission with byte-identical arguments, idempotency key, and protected provenance."
            .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["run_id".to_string()]),
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
