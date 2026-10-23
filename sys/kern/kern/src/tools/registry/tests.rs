use super::*;
use crate::tools::context::ToolInvocation;

struct TestHandler;

#[test]
fn replacing_or_hiding_a_handler_revokes_its_replay_metadata() {
    let mut builder = ToolRegistryBuilder::new();
    builder.register_handler_with_idempotency("probe", Arc::new(TestHandler), true);
    builder.register_handler("probe", Arc::new(TestHandler));
    let (_, registry) = builder.build();
    assert!(!registry.is_idempotent("probe", None));
    let mut builder = ToolRegistryBuilder::new();
    builder.register_handler_with_idempotency("probe", Arc::new(TestHandler), true);
    builder.retain_tools(|_| false);
    let (_, registry) = builder.build();
    assert!(!registry.is_idempotent("probe", None));
}

#[test]
fn replay_metadata_is_affirmative_and_namespace_scoped() {
    let mut builder = ToolRegistryBuilder::new();
    builder.register_handler("unknown", Arc::new(TestHandler));
    builder.register_handler_with_idempotency("remote:probe", Arc::new(TestHandler), true);
    let (_, registry) = builder.build();
    assert!(!registry.is_idempotent("unknown", None));
    assert!(!registry.is_idempotent("probe", None));
    assert!(registry.is_idempotent("probe", Some("remote")));
}

struct ContainerHandler;

impl ToolHandler for ContainerHandler {
    type Output = crate::tools::context::FunctionToolOutput;

    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    fn is_execution_container(&self) -> bool {
        true
    }

    async fn handle(&self, _invocation: ToolInvocation) -> Result<Self::Output, FunctionCallError> {
        unreachable!("classification must not execute the handler")
    }
}

#[test]
fn only_trusted_unqualified_function_handlers_can_omit_leaf_permits() {
    let registry = ToolRegistry::new(HashMap::from([
        (
            "run_workflow".into(),
            Arc::new(TestHandler) as Arc<dyn AnyToolHandler>,
        ),
        (
            "container".into(),
            Arc::new(ContainerHandler) as Arc<dyn AnyToolHandler>,
        ),
        (
            "remote:container".into(),
            Arc::new(ContainerHandler) as Arc<dyn AnyToolHandler>,
        ),
    ]));
    let function = ToolPayload::Function {
        arguments: "{}".into(),
    };
    assert!(!registry.is_execution_container("run_workflow", None, &function));
    assert!(!registry.is_execution_container("missing", None, &function));
    assert!(registry.is_execution_container("container", None, &function));
    assert!(!registry.is_execution_container("container", Some("remote"), &function));
    assert!(!registry.is_execution_container(
        "container",
        None,
        &ToolPayload::Mcp {
            server: "remote".into(),
            tool: "container".into(),
            raw_arguments: "{}".into(),
        }
    ));
}

impl ToolHandler for TestHandler {
    type Output = crate::tools::context::FunctionToolOutput;

    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, _invocation: ToolInvocation) -> Result<Self::Output, FunctionCallError> {
        unreachable!("test handler should not be invoked")
    }
}

#[test]
fn handler_looks_up_namespaced_aliases_explicitly() {
    let plain_handler = Arc::new(TestHandler) as Arc<dyn AnyToolHandler>;
    let namespaced_handler = Arc::new(TestHandler) as Arc<dyn AnyToolHandler>;
    let namespace = "mcp__codex_apps__gmail";
    let tool_name = "gmail_get_recent_emails";
    let namespaced_name = tool_handler_key(tool_name, Some(namespace));
    let registry = ToolRegistry::new(HashMap::from([
        (tool_name.to_string(), Arc::clone(&plain_handler)),
        (namespaced_name, Arc::clone(&namespaced_handler)),
    ]));

    let plain = registry.handler(tool_name, None);
    let namespaced = registry.handler(tool_name, Some(namespace));
    let missing_namespaced = registry.handler(tool_name, Some("mcp__codex_apps__calendar"));

    assert!(missing_namespaced.is_none());
    assert!(
        plain
            .as_ref()
            .is_some_and(|handler| Arc::ptr_eq(handler, &plain_handler))
    );
    assert!(
        namespaced
            .as_ref()
            .is_some_and(|handler| Arc::ptr_eq(handler, &namespaced_handler))
    );
}
