use chaos_ipc::openai_models::ReasoningEffort;
use serde::Deserialize;

use crate::chaos::SessionSettingsUpdate;
use crate::function_tool::FunctionCallError;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::handlers::extract_function_arguments;
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;

#[derive(Deserialize)]
struct SetParentEffortArgs {
    effort: ReasoningEffort,
    #[serde(default)]
    reason: Option<String>,
}

pub struct ParentEffortHandler;

impl ToolHandler for ParentEffortHandler {
    type Output = FunctionToolOutput;

    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<Self::Output, FunctionCallError> {
        let ToolInvocation {
            session,
            turn,
            payload,
            ..
        } = invocation;
        let arguments = extract_function_arguments(payload, "set_parent_effort handler")?;
        let args: SetParentEffortArgs = parse_arguments(&arguments)?;

        if !turn
            .model_info
            .supported_reasoning_levels
            .iter()
            .any(|preset| preset.effort == args.effort)
        {
            let levels = turn
                .model_info
                .supported_reasoning_levels
                .iter()
                .map(|preset| preset.effort.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(FunctionCallError::RespondToModel(format!(
                "effort `{}` is not supported by model `{}`; supported efforts: {levels}",
                args.effort, turn.model_info.slug
            )));
        }

        let collaboration_mode = turn.collaboration_mode.with_updates(
            /*model*/ None,
            Some(Some(args.effort)),
            /*minion_instructions*/ None,
        );
        session
            .update_settings(SessionSettingsUpdate {
                collaboration_mode: Some(collaboration_mode),
                ..Default::default()
            })
            .await
            .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;

        let reason = args
            .reason
            .filter(|reason| !reason.trim().is_empty())
            .map(|reason| format!(" Reason: {reason}"))
            .unwrap_or_default();
        session
            .notify_background_event(
                turn.as_ref(),
                format!(
                    "Parent reasoning effort set to {} for subsequent turns.{reason}",
                    args.effort
                ),
            )
            .await;
        session
            .send_event(
                turn.as_ref(),
                chaos_ipc::protocol::EventMsg::ParentEffortChanged(
                    chaos_ipc::protocol::ParentEffortChangedEvent {
                        effort: args.effort,
                    },
                ),
            )
            .await;

        Ok(FunctionToolOutput::from_text(
            format!(
                "Parent reasoning effort will be {} starting with the next turn.",
                args.effort
            ),
            Some(true),
        ))
    }
}
