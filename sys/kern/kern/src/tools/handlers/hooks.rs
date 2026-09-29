use crate::function_tool::FunctionCallError;
use crate::hooks::{HookAction, HookChangeRequest};
use crate::tools::context::{FunctionToolOutput, ToolInvocation};
use crate::tools::handlers::{extract_function_arguments, parse_arguments};
use crate::tools::registry::{ToolHandler, ToolKind};

pub struct HooksHandler;

impl ToolHandler for HooksHandler {
    type Output = FunctionToolOutput;
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }
    fn is_mutating(
        &self,
        invocation: &ToolInvocation,
    ) -> impl std::future::Future<Output = bool> + Send + '_ {
        let mutating = invocation.tool_name != "hooks_preview";
        async move { mutating }
    }
    async fn handle(&self, invocation: ToolInvocation) -> Result<Self::Output, FunctionCallError> {
        let arguments = extract_function_arguments(invocation.payload, &invocation.tool_name)?;
        let mut value: serde_json::Value = serde_json::from_str(&arguments)
            .map_err(|e| FunctionCallError::RespondToModel(e.to_string()))?;
        let object = value
            .as_object_mut()
            .ok_or_else(|| FunctionCallError::RespondToModel("expected object".into()))?;
        if invocation.tool_name != "hooks_preview" {
            if object.contains_key("action") {
                return Err(FunctionCallError::RespondToModel(
                    "action is only accepted by hooks_preview".into(),
                ));
            }
            let action = match invocation.tool_name.as_str() {
                "hooks_create" => HookAction::Create,
                "hooks_update" => HookAction::Update,
                "hooks_delete" => HookAction::Delete,
                "hooks_set_enabled" => match object.remove("enabled").and_then(|v| v.as_bool()) {
                    Some(true) => HookAction::Enable,
                    Some(false) => HookAction::Disable,
                    None => {
                        return Err(FunctionCallError::RespondToModel(
                            "enabled is required".into(),
                        ));
                    }
                },
                _ => {
                    return Err(FunctionCallError::RespondToModel(
                        "unknown hook operation".into(),
                    ));
                }
            };
            object.insert(
                "action".into(),
                serde_json::to_value(action)
                    .map_err(|e| FunctionCallError::Fatal(e.to_string()))?,
            );
        }
        let request: HookChangeRequest = parse_arguments(&value.to_string())?;
        let change = crate::hooks::prepare(
            &invocation.turn.config.chaos_home,
            &invocation.turn.cwd,
            request,
        )
        .await
        .map_err(|e| FunctionCallError::RespondToModel(e.to_string()))?;
        let output = if invocation.tool_name == "hooks_preview" {
            serde_json::to_value(change).map_err(|e| FunctionCallError::Fatal(e.to_string()))?
        } else {
            let revision = change
                .authorize(&invocation.session, &invocation.turn)
                .await
                .map_err(|e| FunctionCallError::RespondToModel(e.to_string()))?;
            serde_json::json!({"revision": revision, "resource": "chaos://hooks", "applies": "next hook event"})
        };
        Ok(FunctionToolOutput::from_text(
            output.to_string(),
            Some(true),
        ))
    }
}
