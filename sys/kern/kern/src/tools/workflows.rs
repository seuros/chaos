//! Native entry points backed by private, policy-aware SDK leaf adapters.

mod dispatch;
pub(crate) mod executor;
mod external;
mod interactions;
mod sampling;

use crate::client_common::tools::ToolSpec;
use crate::function_tool::FunctionCallError;
use crate::run_observer as observer;
use crate::tools::context::{FunctionToolOutput, ToolInvocation};
use crate::tools::handlers::{extract_function_arguments, parse_arguments};
use crate::tools::registry::{ToolHandler, ToolKind, ToolRegistryBuilder};
use chaos_parrot::sanitize::{JsonSchema, ResponsesApiTool};
#[cfg(test)]
use chaos_synopsis::runs::RunKind;
use chaos_synopsis::runs::{RunId, RunOutcome};
use chaos_synopsis::tree::TreeNode;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;
#[cfg(test)]
use std::time::Duration;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RunArgs {
    tree: TreeNode,
    #[serde(default = "empty_input")]
    input: Value,
    timeout_ms: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StatusArgs {
    run_id: RunId,
}

fn empty_input() -> Value {
    json!({})
}

pub(crate) fn register(builder: &mut ToolRegistryBuilder) {
    let object = || JsonSchema::Object {
        properties: BTreeMap::new(),
        required: None,
        additional_properties: Some(true.into()),
    };
    for (name, description, properties, required) in [
        (
            "run_workflow",
            "Execute a session-owned bounded tree. \
             Nodes: sequence/fallback/parallel_all/race {id,children}; \
             timeout {id,timeout_ms,child}; retry {id,attempts,child}, \
             at most 3 total tries and affirmative idempotency required; \
             condition {id,pointer,predicate:{op:exists|equals,value?}}; \
             call {id,tool,arguments,bindings:[{source,target}],result_key?}; \
             ask {id,message,schema?,result_key?}; \
             sample {id,prompt,system?,max_tokens?,result_key?}. \
             IDs/result keys unique; input reserved. \
             Bindings read JSON Pointers from input/prior results. \
             Limits:128 nodes,depth16,16 concurrent leaves,256 attempts. \
             Calls retain live caller policy; no nested runners/control changes. \
             Unsupported interactions fail normally. Default timeout120s,max1h. No restart replay.",
            BTreeMap::from([
                ("tree".into(), object()),
                ("input".into(), object()),
                (
                    "timeout_ms".into(),
                    JsonSchema::Integer {
                        description: Some("Positive milliseconds, at most 3600000.".into()),
                    },
                ),
            ]),
            vec!["tree".into()],
        ),
        (
            "workflow_status",
            "Inspect an owned workflow/synopsis run; IDs grant no cross-session access.",
            BTreeMap::from([("run_id".into(), JsonSchema::String { description: None })]),
            vec!["run_id".into()],
        ),
        (
            "cancel_workflow",
            "Cancel an owned workflow/synopsis run. \
             Cancelling is not terminal until cleanup completes.",
            BTreeMap::from([("run_id".into(), JsonSchema::String { description: None })]),
            vec!["run_id".into()],
        ),
    ] {
        builder.push_spec_with_parallel_support(
            ToolSpec::Function(ResponsesApiTool {
                name: name.into(),
                description: description.into(),
                strict: false,
                defer_loading: None,
                parameters: JsonSchema::Object {
                    properties,
                    required: Some(required),
                    additional_properties: Some(false.into()),
                },
                output_schema: None,
            }),
            true,
        );
        builder.register_handler(name, Arc::new(WorkflowHandler));
    }
}

struct WorkflowHandler;

/// Do not duplicate step outputs or caller input into the parent tool log.
struct WorkflowOutput {
    text: String,
    success: bool,
}

impl crate::tools::context::ToolOutput for WorkflowOutput {
    fn log_preview(&self) -> String {
        "[workflow snapshot redacted]".into()
    }
    fn success_for_logging(&self) -> bool {
        self.success
    }
    fn to_response_item(
        &self,
        call_id: &str,
        payload: &crate::tools::context::ToolPayload,
    ) -> chaos_ipc::models::ResponseInputItem {
        crate::tools::context::ToolOutput::to_response_item(
            &FunctionToolOutput::from_text(self.text.clone(), Some(self.success)),
            call_id,
            payload,
        )
    }
}

#[cfg(test)]
impl WorkflowOutput {
    fn into_text(self) -> String {
        self.text
    }
}

impl ToolHandler for WorkflowHandler {
    type Output = WorkflowOutput;
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }
    fn is_execution_container(&self) -> bool {
        true
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<Self::Output, FunctionCallError> {
        let arguments = extract_function_arguments(invocation.payload.clone(), "workflow")?;
        if invocation.tool_name != "run_workflow" {
            let args: StatusArgs = parse_arguments(&arguments)?;
            let registry = invocation.session.workflow_runs();
            let snapshot = if invocation.tool_name == "cancel_workflow" {
                registry.cancel(args.run_id)
            } else {
                registry.status(args.run_id)
            }
            .map_err(failure)?;
            return Ok(WorkflowOutput {
                text: serde_json::to_string(&snapshot).map_err(failure)?,
                success: true,
            });
        }
        let args: RunArgs = parse_arguments(&arguments)?;
        let registry = invocation.session.workflow_runs().clone();
        let snapshot = executor::run(
            invocation,
            args,
            &registry,
            executor::Origin::Native,
            tokio_util::sync::CancellationToken::new(),
        )
        .await?;
        let success =
            snapshot.status == chaos_synopsis::runs::RunStatus::Terminal(RunOutcome::Success);
        Ok(WorkflowOutput {
            text: serde_json::to_string(&snapshot).map_err(failure)?,
            success,
        })
    }
}

fn failure(error: impl std::fmt::Display) -> FunctionCallError {
    FunctionCallError::RespondToModel(error.to_string())
}

#[cfg(test)]
mod tests;
