use std::collections::BTreeMap;

use chaos_parrot::sanitize::{JsonSchema, ResponsesApiTool};
use chaos_proc::{MAX_RECOVERY_CHECKPOINT_BYTES, RecoveryCheckpointNote};

use crate::client_common::tools::ToolSpec;
use crate::function_tool::FunctionCallError;
use crate::tools::context::{FunctionToolOutput, ToolInvocation};
use crate::tools::registry::{ToolHandler, ToolKind};

pub(crate) const NAME: &str = "save_recovery_checkpoint";

pub(crate) fn tool() -> ToolSpec {
    let string = |description: &str| JsonSchema::String {
        description: Some(description.into()),
    };
    let mut properties = BTreeMap::from([(
        "summary".into(),
        string("Concise progress summary. Required."),
    )]);
    for (name, description) in [
        ("next_steps", "Next actions after recovery."),
        ("blockers", "Unresolved blockers."),
        (
            "artifacts",
            "Existing file/commit/resource references, not copied source.",
        ),
        (
            "running_tasks",
            "Owned task IDs and their last known status; no automatic restart.",
        ),
    ] {
        properties.insert(
            name.into(),
            JsonSchema::Array {
                items: Box::new(string(description)),
                description: Some(description.into()),
            },
        );
    }
    ToolSpec::Function(ResponsesApiTool {
        name: NAME.into(),
        description: "Save a session-owned recovery note in the runtime database, \
                      only while a host warning is unresolved. Keep the serialized note \
                      within 16 KiB; omit secrets and copied source. The kernel supplies \
                      ownership, short recovery/checkpoint IDs, time, \
                      and historical health evidence. \
                      Success is returned only after commit. Read chaos://recovery \
                      to reread notes, including after restart. \
                      Save BEFORE wait_for_machine_recovery; do not create checkpoint files. \
                      The newest 32 notes are retained per session."
            .into(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["summary".into()]),
            additional_properties: Some(false.into()),
        },
        output_schema: None,
    })
}

pub(crate) struct Handler;

impl ToolHandler for Handler {
    type Output = FunctionToolOutput;

    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    fn is_recovery_checkpoint_writer(&self) -> bool {
        true
    }

    #[allow(clippy::manual_async_fn)]
    fn is_mutating(&self, _: &ToolInvocation) -> impl Future<Output = bool> + Send + '_ {
        async { true }
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<Self::Output, FunctionCallError> {
        let arguments = super::extract_function_arguments(invocation.payload, NAME)?;
        if arguments.len() > 4 * MAX_RECOVERY_CHECKPOINT_BYTES {
            return Err(FunctionCallError::RespondToModel(
                "Checkpoint arguments exceed 64 KiB.".into(),
            ));
        }
        // Preserve note strings verbatim; metadata fields cannot be supplied by the caller.
        let note: RecoveryCheckpointNote = serde_json::from_str(&arguments).map_err(|error| {
            FunctionCallError::RespondToModel(format!("Invalid checkpoint note: {error}"))
        })?;
        let saved = invocation
            .session
            .save_recovery_checkpoint(&invocation.turn, &invocation.call_id, note)
            .await
            .map_err(|error| FunctionCallError::RespondToModel(error.to_string()))?;
        Ok(FunctionToolOutput::from_text(
            serde_json::json!({
                "status": "saved",
                "checkpoint_id": saved.id,
                "recovery_id": saved.recovery_id,
                "resource_uri": format!("chaos://recovery/{}", saved.id),
            })
            .to_string(),
            Some(true),
        ))
    }
}
