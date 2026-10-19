use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use thiserror::Error;

use super::bindings::{validate_bindings, validate_pointer, validate_target};
use super::{
    MAX_AGENT_JOBS, MAX_BINDINGS, MAX_CONCURRENT_LEAVES, MAX_DEPTH, MAX_LEAF_ATTEMPTS, MAX_NODES,
    MAX_RETRY_ATTEMPTS, MAX_TIMEOUT_MS, TreeNode,
};

/// Preflight information, obtained from trusted dispatch metadata rather than
/// model-supplied claims. Runtime adapters must still recheck every attempt.
pub enum Domain<'a> {
    /// Tool calls require affirmative idempotency metadata when retried.
    Tools {
        /// Pure lookup over the caller's available tool metadata.
        is_idempotent: &'a dyn Fn(&str) -> bool,
    },
    /// Each declared job appears once; retries require explicit replay opt-in.
    Agents {
        /// Job ID to operator/model-supplied explicit `allow_replay`.
        jobs: &'a BTreeMap<String, bool>,
    },
}

/// Machine-readable validation category.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TreeErrorCode {
    /// Empty, untrimmed, or excessively long identifier.
    InvalidId,
    /// More than one node uses this ID.
    DuplicateId,
    /// Multiple leaves would overwrite one result key.
    DuplicateResultKey,
    /// A leaf would overwrite caller input.
    ReservedResultKey,
    /// A composite has no children.
    EmptyComposite,
    /// Node, depth, concurrency, attempt, job, or binding limit exceeded.
    LimitExceeded,
    /// Duration is zero or greater than the maximum.
    InvalidTimeout,
    /// Retry attempt count is outside its bounds.
    InvalidRetry,
    /// A leaf belongs to the other execution domain.
    WrongDomain,
    /// A job reference was not declared.
    UnknownJob,
    /// A declared job appears in more than one leaf.
    RepeatedJob,
    /// A declared job is absent from the complete tree.
    MissingJob,
    /// Replay lacks affirmative safety metadata or explicit opt-in.
    UnsafeRetry,
    /// Substitution pointers are malformed, overlap, or have no writable parent.
    InvalidBinding,
    /// Literal call arguments must be an object.
    InvalidArguments,
    /// Condition pointer is invalid.
    InvalidCondition,
    /// Interaction payload is invalid.
    InvalidInteraction,
    /// A workflow may not launch runners or change caller authority.
    ForbiddenTool,
}

/// A structured, data-free preflight diagnostic.
#[derive(Clone, Debug, PartialEq, Eq, Error, Serialize)]
#[error("{code:?} at node `{node_id}`: {message}")]
pub struct TreeError {
    /// Diagnostic category.
    pub code: TreeErrorCode,
    /// Node involved, or the root ID for a tree-wide error.
    pub node_id: String,
    /// Explanation, excluding argument/result values.
    pub message: String,
}

impl TreeError {
    fn new(node: &TreeNode, code: TreeErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            node_id: node.id().to_owned(),
            message: message.into(),
        }
    }
}

/// Conservative bounds, accounting for all fallback branches and retries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct TreeStats {
    /// Total definition nodes.
    pub nodes: usize,
    /// Maximum root-inclusive depth.
    pub depth: usize,
    /// Worst-case concurrent leaves.
    pub concurrent_leaves: usize,
    /// Worst-case total leaf activations.
    pub leaf_attempts: usize,
}

/// An owned definition that passed complete preflight before any work began.
#[derive(Debug)]
pub struct ValidatedTree {
    root: TreeNode,
    stats: TreeStats,
}

impl ValidatedTree {
    /// Validate the entire definition without executing any leaf.
    pub fn new(root: TreeNode, domain: Domain<'_>) -> Result<Self, TreeError> {
        if let Domain::Agents { jobs } = &domain
            && (jobs.is_empty() || jobs.len() > MAX_AGENT_JOBS)
        {
            return Err(TreeError::new(
                &root,
                TreeErrorCode::LimitExceeded,
                "jobs must contain 1..=16 declarations",
            ));
        }
        let mut validator = Validator {
            domain,
            ids: BTreeSet::new(),
            result_keys: BTreeSet::new(),
            used_jobs: BTreeSet::new(),
            depth: 0,
        };
        let work = validator.visit(&root, 1, false)?;
        if let Domain::Agents { jobs } = &validator.domain
            && let Some(missing) = jobs
                .keys()
                .find(|id| !validator.used_jobs.contains(id.as_str()))
        {
            return Err(TreeError::new(
                &root,
                TreeErrorCode::MissingJob,
                format!("job `{missing}` does not appear in the tree"),
            ));
        }
        let stats = TreeStats {
            nodes: validator.ids.len(),
            depth: validator.depth,
            concurrent_leaves: work.concurrent,
            leaf_attempts: work.attempts,
        };
        Ok(Self { root, stats })
    }

    /// The validated definition, without mutable access.
    pub fn root(&self) -> &TreeNode {
        &self.root
    }

    /// Consume the definition for compilation.
    pub fn into_root(self) -> TreeNode {
        self.root
    }

    /// Conservative execution bounds.
    pub const fn stats(&self) -> TreeStats {
        self.stats
    }
}

struct Validator<'a> {
    domain: Domain<'a>,
    ids: BTreeSet<&'a str>,
    result_keys: BTreeSet<&'a str>,
    used_jobs: BTreeSet<&'a str>,
    depth: usize,
}

#[derive(Clone, Copy)]
struct Work {
    concurrent: usize,
    attempts: usize,
}

impl<'a> Validator<'a> {
    fn visit(&mut self, node: &'a TreeNode, depth: usize, replay: bool) -> Result<Work, TreeError> {
        self.depth = self.depth.max(depth);
        if depth > MAX_DEPTH || self.ids.len() >= MAX_NODES {
            return Err(TreeError::new(
                node,
                TreeErrorCode::LimitExceeded,
                "tree exceeds node or depth limit",
            ));
        }
        if !valid_identifier(node.id()) {
            return Err(TreeError::new(
                node,
                TreeErrorCode::InvalidId,
                "IDs must be nonempty, trimmed, and at most 128 bytes",
            ));
        }
        if !self.ids.insert(node.id()) {
            return Err(TreeError::new(
                node,
                TreeErrorCode::DuplicateId,
                "node IDs must be unique",
            ));
        }
        if let Some(key) = node.result_key() {
            if key == "input" {
                return Err(TreeError::new(
                    node,
                    TreeErrorCode::ReservedResultKey,
                    "`input` is reserved",
                ));
            }
            if !valid_identifier(key) {
                return Err(TreeError::new(
                    node,
                    TreeErrorCode::InvalidId,
                    "result keys must be nonempty, trimmed, and at most 128 bytes",
                ));
            }
            if !self.result_keys.insert(key) {
                return Err(TreeError::new(
                    node,
                    TreeErrorCode::DuplicateResultKey,
                    "result keys must be unique",
                ));
            }
        }
        let work = match node {
            TreeNode::Sequence { children, .. } | TreeNode::Fallback { children, .. } => {
                self.children(node, children, depth, replay, false)?
            }
            TreeNode::ParallelAll { children, .. } | TreeNode::Race { children, .. } => {
                self.children(node, children, depth, replay, true)?
            }
            TreeNode::Timeout {
                timeout_ms, child, ..
            } => {
                if !(1..=MAX_TIMEOUT_MS).contains(timeout_ms) {
                    return Err(TreeError::new(
                        node,
                        TreeErrorCode::InvalidTimeout,
                        "timeout_ms must be positive and at most one hour",
                    ));
                }
                self.visit(child, depth + 1, replay)?
            }
            TreeNode::Retry {
                attempts, child, ..
            } => {
                if !(1..=MAX_RETRY_ATTEMPTS).contains(attempts) {
                    return Err(TreeError::new(
                        node,
                        TreeErrorCode::InvalidRetry,
                        "retry attempts must be 1..=3, including the first try",
                    ));
                }
                let mut work = self.visit(child, depth + 1, replay || *attempts > 1)?;
                work.attempts = work.attempts.saturating_mul(*attempts);
                work
            }
            _ => {
                self.leaf(node, replay)?;
                Work {
                    concurrent: 1,
                    attempts: 1,
                }
            }
        };
        if work.concurrent > MAX_CONCURRENT_LEAVES || work.attempts > MAX_LEAF_ATTEMPTS {
            return Err(TreeError::new(
                node,
                TreeErrorCode::LimitExceeded,
                "tree exceeds concurrent-leaf or total-attempt limit",
            ));
        }
        Ok(work)
    }

    fn children(
        &mut self,
        node: &TreeNode,
        children: &'a [TreeNode],
        depth: usize,
        replay: bool,
        parallel: bool,
    ) -> Result<Work, TreeError> {
        if children.is_empty() {
            return Err(TreeError::new(
                node,
                TreeErrorCode::EmptyComposite,
                "composites must have children",
            ));
        }
        let mut work = Work {
            concurrent: 0,
            attempts: 0,
        };
        for child in children {
            let next = self.visit(child, depth + 1, replay)?;
            work.attempts = work.attempts.saturating_add(next.attempts);
            work.concurrent = if parallel {
                work.concurrent.saturating_add(next.concurrent)
            } else {
                work.concurrent.max(next.concurrent)
            };
        }
        Ok(work)
    }

    fn leaf(&mut self, node: &'a TreeNode, replay: bool) -> Result<(), TreeError> {
        use TreeErrorCode as Code;
        match node {
            TreeNode::Condition { pointer, .. } => validate_pointer(pointer)
                .map_err(|error| TreeError::new(node, Code::InvalidCondition, error.to_string()))?,
            TreeNode::Call {
                tool,
                arguments,
                bindings,
                ..
            } => {
                let Domain::Tools { is_idempotent } = &self.domain else {
                    return Err(TreeError::new(
                        node,
                        Code::WrongDomain,
                        "agent trees only accept job and condition leaves",
                    ));
                };
                if tool.is_empty()
                    || tool.trim() != tool
                    || tool.len() > 512
                    || is_control_tool(tool)
                {
                    return Err(TreeError::new(
                        node,
                        Code::ForbiddenTool,
                        "runner and authority-changing control tools cannot be workflow steps",
                    ));
                }
                if replay && !is_idempotent(tool) {
                    return Err(TreeError::new(
                        node,
                        Code::UnsafeRetry,
                        "retried calls need affirmative idempotent metadata",
                    ));
                }
                if !arguments.is_object() {
                    return Err(TreeError::new(
                        node,
                        Code::InvalidArguments,
                        "literal arguments must be an object",
                    ));
                }
                if bindings.len() > MAX_BINDINGS {
                    return Err(TreeError::new(
                        node,
                        Code::LimitExceeded,
                        "too many argument bindings",
                    ));
                }
                validate_bindings(bindings).map_err(|error| {
                    TreeError::new(node, Code::InvalidBinding, error.to_string())
                })?;
                // Source values may be unavailable until execution. Target
                // structure, however, can be checked against literals now.
                for binding in bindings {
                    validate_target(arguments, &binding.target).map_err(|error| {
                        TreeError::new(node, Code::InvalidBinding, error.to_string())
                    })?;
                }
            }
            TreeNode::Job { job_id, .. } => {
                let Domain::Agents { jobs } = &self.domain else {
                    return Err(TreeError::new(
                        node,
                        Code::WrongDomain,
                        "tool workflows cannot launch agent jobs",
                    ));
                };
                let Some(allow_replay) = jobs.get(job_id) else {
                    return Err(TreeError::new(
                        node,
                        Code::UnknownJob,
                        format!("job `{job_id}` was not declared"),
                    ));
                };
                if !self.used_jobs.insert(job_id) {
                    return Err(TreeError::new(
                        node,
                        Code::RepeatedJob,
                        "each declared job must appear exactly once",
                    ));
                }
                if replay && !allow_replay {
                    return Err(TreeError::new(
                        node,
                        Code::UnsafeRetry,
                        "retried jobs require allow_replay=true",
                    ));
                }
            }
            TreeNode::Ask { message, .. }
            | TreeNode::Sample {
                prompt: message, ..
            } => {
                if !matches!(self.domain, Domain::Tools { .. }) {
                    return Err(TreeError::new(
                        node,
                        Code::WrongDomain,
                        "agent trees only accept job and condition leaves",
                    ));
                }
                if replay {
                    return Err(TreeError::new(
                        node,
                        Code::UnsafeRetry,
                        "Ask/Sample cannot be replayed",
                    ));
                }
                if message.trim().is_empty()
                    || matches!(node, TreeNode::Sample { max_tokens: 0, .. })
                {
                    return Err(TreeError::new(
                        node,
                        Code::InvalidInteraction,
                        "interaction text and output-token limit must be nonempty",
                    ));
                }
            }
            _ => unreachable!("composites are validated by visit"),
        }
        Ok(())
    }
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && value.trim() == value
}

/// Deny known control surfaces, including namespaced forms. An adapter must
/// also check the resolved canonical tool, so aliases cannot grant authority.
pub fn is_control_tool(name: &str) -> bool {
    let name = name.rsplit("__").next().unwrap_or(name);
    let name = name.rsplit(['.', ':']).next().unwrap_or(name);
    matches!(
        name,
        "run_workflow"
            | "workflow_status"
            | "cancel_workflow"
            | "run_synopsis"
            | "start_attested_review"
            | "resume_attested_review"
            | "cancel_attested_review"
            | "spawn_agent"
            | "send_input"
            | "resume_agent"
            | "close_agent"
            | "spawn_minions_on_csv"
            | "enable_tools"
            | "disable_tools"
            | "switch_mode"
            | "request_permissions"
            | "hooks_create"
            | "hooks_update"
            | "hooks_set_enabled"
            | "hooks_delete"
            | "mcp_add_server"
            | "mcp_server"
            | "wait_for_machine_recovery"
    )
}
