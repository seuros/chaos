use std::sync::Arc;

use crate::chaos::make_session_and_context;
use crate::tools::context::ToolPayload;
use chaos_ipc::models::ResponseItem;

use super::ToolRouter;

struct PolicyHandler;

impl crate::tools::registry::ToolHandler for PolicyHandler {
    type Output = crate::tools::context::FunctionToolOutput;
    fn kind(&self) -> crate::tools::registry::ToolKind {
        crate::tools::registry::ToolKind::Function
    }
    async fn handle(
        &self,
        _: crate::tools::context::ToolInvocation,
    ) -> Result<Self::Output, crate::function_tool::FunctionCallError> {
        unreachable!("policy checks must precede execution")
    }
}

fn policy_router(idempotent: bool) -> ToolRouter {
    let mut builder = crate::tools::registry::ToolRegistryBuilder::new();
    builder.register_handler_with_idempotency("probe", Arc::new(PolicyHandler), idempotent);
    let (specs, registry) = builder.build();
    ToolRouter {
        registry,
        specs,
        model_visible_specs: vec![],
    }
}

#[test]
fn workflow_guard_checks_resolved_names_visibility_and_current_replay_metadata() {
    let mut call = super::ToolCall {
        tool_name: "probe".into(),
        tool_namespace: None,
        call_id: "leaf".into(),
        payload: ToolPayload::Function {
            arguments: "{}".into(),
        },
    };
    assert!(
        policy_router(false)
            .check_workflow_call(&call, false)
            .is_ok()
    );
    assert!(
        policy_router(false)
            .check_workflow_call(&call, true)
            .is_err()
    );
    assert!(policy_router(true).check_workflow_call(&call, true).is_ok());
    call.payload = ToolPayload::Function {
        arguments: r#"{"_meta":{"idempotentHint":true,"source":"direct","replay":false}}"#.into(),
    };
    assert!(
        policy_router(false)
            .check_workflow_call(&call, true)
            .is_err()
    );
    call.tool_namespace = Some("unregistered".into());
    assert!(
        policy_router(true)
            .check_workflow_call(&call, false)
            .is_err()
    );
    call.tool_namespace = None;
    call.payload = ToolPayload::Mcp {
        server: "remote".into(),
        tool: "run_synopsis".into(),
        raw_arguments: "{}".into(),
    };
    assert!(
        policy_router(true)
            .check_workflow_call(&call, false)
            .is_err()
    );
    call.tool_name = "alias.switch_mode".into();
    assert!(
        policy_router(true)
            .check_workflow_call(&call, false)
            .is_err()
    );
}

#[tokio::test]
async fn build_tool_call_uses_namespace_for_registry_name() -> anyhow::Result<()> {
    let (session, _) = make_session_and_context().await;
    let session = Arc::new(session);
    let tool_name = "create_event".to_string();

    let call = ToolRouter::build_tool_call(
        &session,
        ResponseItem::FunctionCall {
            id: None,
            name: tool_name.clone(),
            namespace: Some("mcp__codex_apps__calendar".to_string()),
            arguments: "{}".to_string(),
            call_id: "call-namespace".to_string(),
            provider_metadata: None,
        },
    )
    .await?
    .expect("function_call should produce a tool call");

    assert_eq!(call.tool_name, tool_name);
    assert_eq!(
        call.tool_namespace,
        Some("mcp__codex_apps__calendar".to_string())
    );
    assert_eq!(call.call_id, "call-namespace");
    match call.payload {
        ToolPayload::Function { arguments } => {
            assert_eq!(arguments, "{}");
        }
        other => panic!("expected function payload, got {other:?}"),
    }

    Ok(())
}
