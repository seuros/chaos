//! Translate checked input, without introducing an execution loop or scheduler.

use std::sync::Arc;
use std::time::Duration;

use mcp_host::bt::{Ask, Call, Check, Limit, Node, Sample, Step, Threshold};
use serde_json::json;

use super::{Domain, TreeError, TreeErrorCode, TreeNode, ValidatedTree, resolve_arguments};

/// Preflight the complete tool tree, then compile it to upstream SDK steps.
///
/// This only constructs a definition: it performs no calls and registers no
/// tools. The caller must use a private registry of policy-aware dispatch
/// adapters, rechecking live authority and retry safety on every activation.
/// The idempotency lookup here is trusted preflight metadata, not authority.
/// Attach the owning run's observer/interactions to the resulting `TreeTool`;
/// its existing Scope/Job machinery owns execution and cancellation.
pub fn compile_workflow(
    root: TreeNode,
    is_idempotent: &dyn Fn(&str) -> bool,
) -> Result<Node<Step>, TreeError> {
    let checked = ValidatedTree::new(root, Domain::Tools { is_idempotent })?;
    compile(checked.into_root())
}

fn compile(node: TreeNode) -> Result<Node<Step>, TreeError> {
    Ok(match node {
        TreeNode::Sequence { children, .. } => Node::sequence(compile_children(children)?),
        TreeNode::Fallback { children, .. } => Node::fallback(compile_children(children)?),
        TreeNode::ParallelAll { children, .. } => Node::parallel(
            compile_children(children)?,
            Threshold::All,
            Threshold::Count(1),
        ),
        TreeNode::Race { children, .. } => Node::parallel(
            compile_children(children)?,
            Threshold::Count(1),
            Threshold::Count(1),
        ),
        TreeNode::Timeout {
            timeout_ms, child, ..
        } => compile(*child)?.timeout(Duration::from_millis(timeout_ms)),
        TreeNode::Retry {
            attempts, child, ..
        } => compile(*child)?.retry_until_successful(Limit::Times(attempts)),
        TreeNode::Condition {
            id,
            pointer,
            predicate,
        } => {
            let id = Arc::<str>::from(id);
            Step::from(Check::new(Arc::clone(&id), move |board| {
                predicate.matches(board, &pointer)
            }))
            .with_id(id)
            .into()
        }
        TreeNode::Call {
            id,
            tool,
            arguments,
            bindings,
            result_key,
        } => {
            let id = Arc::<str>::from(id);
            Step::from(
                Call::new(tool)
                    .try_args_from(move |board| {
                        resolve_arguments(&arguments, &bindings, board)
                            .map_err(|error| json!({"binding_error": error}).to_string())
                    })
                    .store_as(result_key_or(result_key, &id)),
            )
            .with_id(id)
            .into()
        }
        TreeNode::Ask {
            id,
            message,
            schema,
            result_key,
        } => {
            let id = Arc::<str>::from(id);
            let mut ask = Ask::new(message).store_as(result_key_or(result_key, &id));
            if let Some(schema) = schema {
                ask = ask.with_schema(schema);
            }
            Step::from(ask).with_id(id).into()
        }
        TreeNode::Sample {
            id,
            prompt,
            system,
            max_tokens,
            result_key,
        } => {
            let id = Arc::<str>::from(id);
            let mut sample = Sample::new(prompt)
                .with_max_tokens(max_tokens)
                .store_as(result_key_or(result_key, &id));
            if let Some(system) = system {
                sample = sample.with_system(system);
            }
            Step::from(sample).with_id(id).into()
        }
        TreeNode::Job { id, .. } => {
            return Err(TreeError {
                code: TreeErrorCode::WrongDomain,
                node_id: id,
                message: "agent jobs require the agent execution adapter".into(),
            });
        }
    })
}

fn compile_children(children: Vec<TreeNode>) -> Result<Vec<Node<Step>>, TreeError> {
    children.into_iter().map(compile).collect()
}

fn result_key_or(result_key: Option<String>, id: &Arc<str>) -> Arc<str> {
    result_key.map_or_else(|| Arc::clone(id), Arc::from)
}
