use std::collections::BTreeMap;

use chaos_parrot::sanitize::{JsonSchema, ResponsesApiTool};
use serde::Deserialize;

use crate::client_common::tools::ToolSpec;
use crate::function_tool::FunctionCallError;
use crate::tools::context::{FunctionToolOutput, ToolInvocation};
use crate::tools::registry::{ToolHandler, ToolKind};

pub(crate) const NAME: &str = "wait_for_machine_recovery";

pub(crate) fn tool() -> ToolSpec {
    ToolSpec::Function(ResponsesApiTool {
        name: NAME.into(),
        description: "Opt into one recovery wake and park this turn. First checkpoint essential work and stop/check your heavy background processes. Call this tool ALONE in the response. The kernel waits for all outstanding host warnings to recover with configured headroom and a stable observation window, then wakes you to reassess, not blindly restart. No processes are stopped by this tool. New operator input cancels the wait; waits do not survive session closure or restart.".into(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::Object {
            properties: BTreeMap::new(),
            required: None,
            additional_properties: Some(false.into()),
        },
        output_schema: None,
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {}

pub(crate) struct Handler;

impl ToolHandler for Handler {
    type Output = FunctionToolOutput;

    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    // Session scheduling control, not host filesystem mutation.
    #[allow(clippy::manual_async_fn)]
    fn is_mutating(&self, _: &ToolInvocation) -> impl Future<Output = bool> + Send + '_ {
        async { false }
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<Self::Output, FunctionCallError> {
        let arguments = super::extract_function_arguments(invocation.payload, NAME)?;
        let _: Args = super::parse_arguments(&arguments)?;
        if !invocation.turn.config.machine_warnings.enabled {
            return Err(FunctionCallError::RespondToModel(
                "Machine warnings are disabled.".into(),
            ));
        }
        let mut state = invocation.session.state.lock().await;
        let id = state
            .machine_recovery
            .request_wait(&invocation.turn.sub_id)
            .map_err(FunctionCallError::RespondToModel)?;
        Ok(FunctionToolOutput::from_text(serde_json::json!({
            "wait_id": id,
            "status": "requested",
            "message": "The runner parks after this result only if this was the sole tool call. Recovery wakes you to reassess; existing processes are not stopped. This wait lasts only for this live session."
        }).to_string(), Some(true)))
    }
}
