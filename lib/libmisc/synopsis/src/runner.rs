use std::any::Any;
use std::collections::HashSet;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::Poll;

use futures::FutureExt;
use mcp_host::bt::{self, Job, Leaf, Node as BtNode, Scope, Status, Threshold, Time, Tree};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::{ActionId, ActionOutcome, CompositeKind, Node, Outcome, Synopsis, ValidationError};

/// Boxed asynchronous action returned by [`ActionExecutor`].
pub type ActionFuture = Pin<Box<dyn Future<Output = ActionOutcome> + Send + 'static>>;

/// Backend responsible for executing synopsis action leaves.
///
/// Actions run concurrently on the task driving the tree and must not block
/// it. The runner wraps the returned future in a cancellation select and drops
/// abandoned work even if the executor does not observe the supplied token.
pub trait ActionExecutor<A>: Send + Sync + 'static {
    /// Starts one action.
    fn execute(&self, id: ActionId, action: A, cancellation: CancellationToken) -> ActionFuture;
}

/// Error returned while running a validated synopsis.
#[derive(Debug, Error)]
pub enum RunError {
    /// An executor panicked while creating or polling its action future.
    #[error("action `{id}` panicked: {message}")]
    ActionPanicked {
        /// The action that panicked.
        id: ActionId,
        /// The panic message, when the payload contains one.
        message: String,
    },
}

/// Executes one validated [`Synopsis`].
///
/// A runner is single-use. Each action ID may be launched at most once.
pub struct Runner<A> {
    tree: Tree<ActionLeaf>,
    context: Context<A>,
}

struct Context<A> {
    actions: Vec<ActionState<A>>,
    executor: Arc<dyn ActionExecutor<A>>,
    scope: Scope<'static>,
    error: Arc<Mutex<Option<ActionPanic>>>,
}

struct ActionPanic {
    index: usize,
    message: String,
}

struct ActionState<A> {
    id: ActionId,
    action: Option<A>,
    job: Job<Status>,
    cancellation: CancellationToken,
    finished: Option<Status>,
}

/// Immutable leaf definition; activation state belongs to the context.
struct ActionLeaf(usize);

impl<A: Send + 'static> Leaf<Context<A>> for ActionLeaf {
    fn tick(&self, context: &mut Context<A>, _now: Time) -> Status {
        // A panic is a runner error, never a recoverable fallback result.
        // Scope may have polled multiple jobs before this tick.
        if lock_error(&context.error).is_some() {
            return Status::Failure;
        }

        let state = &mut context.actions[self.0];
        if let Some(status) = state.finished {
            return status;
        }

        let index = self.0;
        let status = state.job.poll(&context.scope, || {
            let action = state.action.take();
            let id = state.id.clone();
            let executor = Arc::clone(&context.executor);
            let cancellation = state.cancellation.clone();
            let error = Arc::clone(&context.error);
            // Capture before polling, so abandoning even an unpolled job
            // delivers cancellation. Never capture Scope in its own jobs.
            let cancel_on_drop = cancellation.clone().drop_guard();
            async move {
                let result = AssertUnwindSafe(async {
                    // Compilation and the finished cache ensure single use.
                    let Some(action) = action else {
                        unreachable!("a single-use action cannot be launched twice");
                    };
                    let work = executor.execute(id, action, cancellation.clone());
                    tokio::select! {
                        biased;
                        () = cancellation.cancelled() => Status::Failure,
                        outcome = work => match outcome {
                            ActionOutcome::Success => Status::Success,
                            ActionOutcome::Failure => Status::Failure,
                        },
                    }
                })
                .catch_unwind()
                .await;

                match result {
                    Ok(status) => {
                        cancel_on_drop.disarm();
                        status
                    }
                    Err(payload) => {
                        let mut error = lock_error(&error);
                        if error.is_none() {
                            *error = Some(ActionPanic {
                                index,
                                message: panic_message(payload),
                            });
                        }
                        Status::Failure
                    }
                }
            }
        });

        match status {
            Poll::Pending => Status::Running,
            Poll::Ready(status) => {
                state.finished = Some(status);
                status
            }
        }
    }

    fn halt(&self, context: &mut Context<A>) {
        let state = &mut context.actions[self.0];
        state.cancellation.cancel();
        state.job.abort();
        // A halted action must not be restarted by another branch.
        state.finished = Some(Status::Failure);
    }
}

impl<A: Send + 'static> Runner<A> {
    /// Validates and compiles a synopsis.
    pub fn new(
        synopsis: Synopsis<A>,
        executor: Arc<dyn ActionExecutor<A>>,
    ) -> Result<Self, ValidationError> {
        validate_node(synopsis.root(), &mut HashSet::new())?;
        let mut actions = Vec::new();
        let root = compile_node(synopsis.into_root(), &mut actions);
        Ok(Self {
            tree: Tree::new(root)?,
            context: Context {
                actions,
                executor,
                scope: Scope::default(),
                error: Arc::default(),
            },
        })
    }

    /// Runs until success, failure, external cancellation, or an executor
    /// panic. Dropping this future halts the tree and drops all owned jobs.
    pub async fn run(
        mut self,
        external_cancellation: CancellationToken,
    ) -> Result<Outcome, RunError> {
        let scope = self.context.scope.clone();
        let outcome = tokio::select! {
            biased;
            () = external_cancellation.cancelled() => Outcome::Cancelled,
            status = bt::run(&mut self.tree, &mut self.context, &scope) => match status {
                Status::Success => Outcome::Success,
                Status::Failure => Outcome::Failure,
                Status::Running => unreachable!("bt::run returns only terminal status"),
            },
        };
        // bt::run halts active leaves on cancellation/drop. Dropping this
        // context and the final Scope handle releases every aborted future
        // before returning; there are no detached action tasks to drain.
        let panic = lock_error(&self.context.error).take();
        match panic {
            Some(ActionPanic { index, message }) => Err(RunError::ActionPanicked {
                id: self.context.actions.swap_remove(index).id,
                message,
            }),
            None => Ok(outcome),
        }
    }
}

fn lock_error(error: &Mutex<Option<ActionPanic>>) -> MutexGuard<'_, Option<ActionPanic>> {
    error.lock().unwrap_or_else(PoisonError::into_inner)
}

fn panic_message(payload: Box<dyn Any + Send>) -> String {
    match payload.downcast::<String>() {
        Ok(message) => *message,
        Err(payload) => payload.downcast_ref::<&str>().map_or_else(
            || "non-string panic payload".to_owned(),
            |message| (*message).to_owned(),
        ),
    }
}

fn validate_node<'a, A>(
    node: &'a Node<A>,
    ids: &mut HashSet<&'a ActionId>,
) -> Result<(), ValidationError> {
    let (kind, children) = match node {
        Node::Action { id, .. } => {
            if id.as_str().trim().is_empty() {
                return Err(ValidationError::EmptyActionId);
            }
            if !ids.insert(id) {
                return Err(ValidationError::DuplicateActionId(id.clone()));
            }
            return Ok(());
        }
        Node::Sequence(children) => (CompositeKind::Sequence, children),
        Node::Fallback(children) => (CompositeKind::Fallback, children),
        Node::ParallelAll(children) => (CompositeKind::ParallelAll, children),
        Node::Race(children) => (CompositeKind::Race, children),
    };
    if children.is_empty() {
        return Err(ValidationError::EmptyComposite(kind));
    }
    children
        .iter()
        .try_for_each(|child| validate_node(child, ids))
}

fn compile_node<A>(node: Node<A>, actions: &mut Vec<ActionState<A>>) -> BtNode<ActionLeaf> {
    let mut compile_children = |children: Vec<Node<A>>| {
        children
            .into_iter()
            .map(|child| compile_node(child, actions))
            .collect::<Vec<_>>()
    };
    match node {
        Node::Action { id, action } => {
            let index = actions.len();
            actions.push(ActionState {
                id,
                action: Some(action),
                job: Job::default(),
                cancellation: CancellationToken::new(),
                finished: None,
            });
            BtNode::leaf(ActionLeaf(index))
        }
        Node::Sequence(children) => BtNode::sequence(compile_children(children)),
        Node::Fallback(children) => BtNode::fallback(compile_children(children)),
        Node::ParallelAll(children) => BtNode::parallel(
            compile_children(children),
            Threshold::All,
            Threshold::Count(1),
        ),
        // Both terminal outcomes decide a race, with upstream's leftmost
        // child deciding a tie when several jobs finish before one tick.
        Node::Race(children) => BtNode::parallel(
            compile_children(children),
            Threshold::Count(1),
            Threshold::Count(1),
        ),
    }
}
