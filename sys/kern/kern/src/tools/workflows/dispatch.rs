use super::{failure, observer::Observer};
use crate::client_common::tools::ToolSpec;
use crate::function_tool::FunctionCallError;
use crate::tools::context::ToolInvocation;
use crate::tools::parallel::ToolCallRuntime;
use crate::tools::router::{ToolCallSource, ToolRouter};
use chaos_ipc::models::{ResponseInputItem, ResponseItem};
use chaos_parrot::sanitize::JsonSchema;
use chaos_synopsis::runs::RunId;
use chaos_synopsis::tree::{Domain, TreeNode, ValidatedTree, compile_workflow};
use mcp_host::bt::{Node, Step};
use mcp_host::prelude::{ExecutionContext, ToolError, ToolOutput};
use mcp_host::registry::tools::{Tool, ToolFuture};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub(super) struct Prepared {
    pub(super) root: Node<Step>,
    pub(super) leaves: Vec<Leaf>,
}
impl Prepared {
    pub(super) fn display_names(&self) -> BTreeMap<String, String> {
        self.leaves
            .iter()
            .map(|leaf| (leaf.alias.clone(), leaf.tool.clone()))
            .collect()
    }
}
pub(super) struct Leaf {
    alias: String,
    id: String,
    tool: String,
    replay: bool,
    idempotent: bool,
    schema: Value,
}

pub(super) fn prepare(root: TreeNode, router: &ToolRouter) -> Result<Prepared, FunctionCallError> {
    let mut root = ValidatedTree::new(
        root,
        Domain::Tools {
            is_idempotent: &|name| router.tool_is_idempotent(name),
        },
    )
    .map_err(failure)?
    .into_root();
    let schemas: BTreeMap<&str, &JsonSchema> = router
        .visible_specs()
        .iter()
        .filter_map(|spec| {
            if let ToolSpec::Function(tool) = spec {
                Some((tool.name.as_str(), &tool.parameters))
            } else {
                None
            }
        })
        .collect();
    let mut leaves = Vec::new();
    rewrite(&mut root, false, router, &schemas, &mut leaves)?;
    let root = compile_workflow(root, &|alias| {
        leaves
            .iter()
            .any(|leaf| leaf.alias == alias && leaf.idempotent)
    })
    .map_err(failure)?;
    Ok(Prepared { root, leaves })
}

fn rewrite(
    node: &mut TreeNode,
    replay: bool,
    router: &ToolRouter,
    schemas: &BTreeMap<&str, &JsonSchema>,
    leaves: &mut Vec<Leaf>,
) -> Result<(), FunctionCallError> {
    match node {
        TreeNode::Sequence { children, .. }
        | TreeNode::Fallback { children, .. }
        | TreeNode::ParallelAll { children, .. }
        | TreeNode::Race { children, .. } => {
            for child in children {
                rewrite(child, replay, router, schemas, leaves)?;
            }
        }
        TreeNode::Timeout { child, .. } => rewrite(child, replay, router, schemas, leaves)?,
        TreeNode::Retry { child, .. } => rewrite(child, true, router, schemas, leaves)?,
        TreeNode::Call { id, tool, .. } => {
            // Native Ask legitimately dispatches this tool through Caller.
            // An explicit call leaf must not select the native mailbox on an
            // external run: interactions belong to the origin-aware Ask bridge.
            let base = tool.rsplit("__").next().unwrap_or(tool);
            if base.rsplit(['.', ':']).next() == Some("request_user_input") {
                return Err(failure(
                    "use an ask leaf instead of calling request_user_input",
                ));
            }
            let schema = schemas
                .get(tool.as_str())
                .ok_or_else(|| failure("workflow tool is unavailable"))?;
            let schema = serde_json::to_value(schema).map_err(failure)?;
            let alias = format!("leaf_{}", leaves.len());
            let idempotent = router.tool_is_idempotent(tool);
            let tool = std::mem::replace(tool, alias.clone());
            leaves.push(Leaf {
                alias,
                id: id.clone(),
                tool,
                replay,
                idempotent,
                schema,
            });
        }
        _ => {}
    }
    Ok(())
}

pub(super) struct LeafAdapter {
    pub(super) leaf: Leaf,
    pub(super) observer: Arc<Observer>,
    pub(super) caller: Caller,
}

#[derive(Clone)]
pub(super) struct Caller {
    pub(super) invocation: Arc<ToolInvocation>,
    pub(super) cancellation: CancellationToken,
    pub(super) run_id: RunId,
    pub(super) tasks: tokio_util::task::TaskTracker,
}
impl Tool for LeafAdapter {
    fn name(&self) -> &str {
        &self.leaf.alias
    }
    fn input_schema(&self) -> Value {
        self.leaf.schema.clone()
    }
    fn execute<'a>(&'a self, ctx: ExecutionContext<'a>) -> ToolFuture<'a> {
        Box::pin(async move {
            let attempt = self
                .observer
                .attempt(&self.leaf.id)
                .ok_or_else(|| ToolError::Execution("missing host step activation".into()))?;
            self.caller
                .call(
                    &self.leaf.tool,
                    &self.leaf.id,
                    attempt,
                    self.leaf.replay,
                    ctx.params.clone(),
                )
                .await
        })
    }
}

impl Caller {
    pub(super) async fn call(
        &self,
        tool: &str,
        node_id: &str,
        attempt: usize,
        replay: bool,
        params: Value,
    ) -> Result<ToolOutput, ToolError> {
        if self.cancellation.is_cancelled() {
            return Err(ToolError::Execution("workflow cancelled".into()));
        }
        let original = &self.invocation;
        let turn = original
            .session
            .try_effective_turn_context(&original.turn)
            .await
            .map_err(|error| ToolError::Execution(error.to_string()))?;
        let router = crate::chaos::built_tools(&original.session, &turn, &[], &self.cancellation)
            .await
            .map_err(|error| ToolError::Execution(error.to_string()))?;
        let call = ToolRouter::build_tool_call(
            &original.session,
            ResponseItem::FunctionCall {
                id: None,
                name: tool.into(),
                namespace: None,
                arguments: params.to_string(),
                call_id: format!(
                    "{}/{}/{}/{}",
                    original.call_id, self.run_id, node_id, attempt
                ),
                provider_metadata: None,
            },
        )
        .await
        .map_err(|error| ToolError::Execution(error.to_string()))?
        .ok_or_else(|| ToolError::Execution("unsupported workflow call".into()))?;
        let result = ToolCallRuntime::new(
            router,
            original.session.clone(),
            turn,
            original.tracker.clone(),
        )
        .handle_tool_call_with_source(
            call,
            ToolCallSource::Workflow {
                replay,
                tasks: self.tasks.clone(),
            },
            self.cancellation.clone(),
        )
        .await
        .map_err(|error| ToolError::Execution(error.to_string()))?;
        if !result.result.success_for_logging() {
            return Err(ToolError::Execution(result.result.log_preview()));
        }
        let value = match result.into_response() {
            ResponseInputItem::McpToolCallOutput { output, .. } => output
                .structured_content
                .unwrap_or_else(|| json!({"content":output.content})),
            ResponseInputItem::FunctionCallOutput { output, .. }
            | ResponseInputItem::CustomToolCallOutput { output, .. } => {
                match output.body.to_text() {
                    Some(text) => {
                        serde_json::from_str(&text).unwrap_or_else(|_| json!({"text":text}))
                    }
                    None => json!({"content":output}),
                }
            }
            _ => return Err(ToolError::Execution("unsupported workflow output".into())),
        };
        match value {
            Value::Object(value) => Ok(ToolOutput::json(value)),
            value => Ok(ToolOutput::json(serde_json::Map::from_iter([(
                "value".into(),
                value,
            )]))),
        }
    }
}
