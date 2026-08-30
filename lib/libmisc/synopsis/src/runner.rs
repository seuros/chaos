use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use bonsai_bt::{
    Action, BT, Behavior, Event, Failure, Race, Running, Select, Sequence, Status, Success, WhenAll,
};
use thiserror::Error;
use tokio::task::{JoinError, JoinSet};
use tokio_util::sync::CancellationToken;

use crate::{ActionId, ActionOutcome, CompositeKind, Node, Outcome, Synopsis, ValidationError};

const MAX_STABILIZATION_TICKS: usize = 64;

/// Boxed asynchronous action returned by [`ActionExecutor`].
pub type ActionFuture = Pin<Box<dyn Future<Output = ActionOutcome> + Send + 'static>>;

/// Backend responsible for executing synopsis action leaves.
///
/// The runner also wraps the returned future in a cancellation select. This
/// ensures that cancellation drops the future even if an executor does not
/// actively observe the supplied token.
pub trait ActionExecutor<A>: Send + Sync + 'static {
    /// Starts one action.
    fn execute(&self, id: ActionId, action: A, cancellation: CancellationToken) -> ActionFuture;
}

/// Error returned while running a validated synopsis.
#[derive(Debug, Error)]
pub enum RunError {
    /// Bonsai unexpectedly reported that an already-finished tree was ticked.
    #[error("behavior tree stopped producing a tick result before termination")]
    UnexpectedFinishedTree,
    /// The behavior tree requested an action that was not in the compiled
    /// action table.
    #[error("behavior tree referenced missing action `{0}`")]
    MissingAction(ActionId),
    /// The synopsis remained running without any asynchronous work capable of
    /// waking it.
    #[error("synopsis is running but has no active action")]
    Stalled,
    /// Repeated zero-time ticks could not settle on a stable active branch.
    #[error("behavior tree did not stabilize within {MAX_STABILIZATION_TICKS} ticks")]
    StabilizationLimit,
    /// An action task panicked or was aborted unexpectedly.
    #[error("action task failed to join: {0}")]
    TaskJoin(#[source] JoinError),
}

enum TaskExit {
    Completed(ActionOutcome),
    Cancelled,
}

/// Executes one validated [`Synopsis`].
///
/// A runner is single-use. Each action ID may be launched at most once.
pub struct Runner<A> {
    tree: BT<ActionId, ()>,
    actions: HashMap<ActionId, A>,
    executor: Arc<dyn ActionExecutor<A>>,
    active: HashMap<ActionId, CancellationToken>,
    completed: HashMap<ActionId, ActionOutcome>,
    abandoned: HashSet<ActionId>,
    tasks: JoinSet<(ActionId, TaskExit)>,
}

impl<A> Runner<A>
where
    A: Send + 'static,
{
    /// Validates and compiles a synopsis.
    pub fn new(
        synopsis: Synopsis<A>,
        executor: Arc<dyn ActionExecutor<A>>,
    ) -> Result<Self, ValidationError> {
        let mut actions = HashMap::new();
        let behavior = compile_node(synopsis.into_root(), &mut actions)?;

        Ok(Self {
            tree: BT::new(behavior, ()),
            actions,
            executor,
            active: HashMap::new(),
            completed: HashMap::new(),
            abandoned: HashSet::new(),
            tasks: JoinSet::new(),
        })
    }

    /// Runs the synopsis until success, failure, external cancellation, or a
    /// runtime error.
    pub async fn run(
        mut self,
        external_cancellation: CancellationToken,
    ) -> Result<Outcome, RunError> {
        loop {
            if external_cancellation.is_cancelled() {
                self.cancel_and_drain_all().await?;
                return Ok(Outcome::Cancelled);
            }

            match self.stabilize()? {
                Success => {
                    self.cancel_and_drain_all().await?;
                    return Ok(Outcome::Success);
                }
                Failure => {
                    self.cancel_and_drain_all().await?;
                    return Ok(Outcome::Failure);
                }
                Running => {}
            }

            if self.active.is_empty() {
                self.cancel_and_drain_all().await?;
                return Err(RunError::Stalled);
            }

            tokio::select! {
                biased;
                _ = external_cancellation.cancelled() => {
                    self.cancel_and_drain_all().await?;
                    return Ok(Outcome::Cancelled);
                }
                task = self.tasks.join_next() => {
                    match task {
                        Some(Ok(exit)) => self.record_task_exit(exit),
                        Some(Err(error)) => {
                            self.cancel_and_drain_after_error().await;
                            return Err(RunError::TaskJoin(error));
                        }
                        None => {
                            self.cancel_and_drain_after_error().await;
                            return Err(RunError::Stalled);
                        }
                    }
                }
            }
        }
    }

    fn stabilize(&mut self) -> Result<Status, RunError> {
        let mut previous_visited = None;

        // Bonsai reports leaf status but does not expose a branch-abandoned
        // callback. Re-tick without advancing time until the active leaf set
        // settles, allowing actions left behind by a completed nested race or
        // composite transition to be cancelled before the runner waits again.
        for _ in 0..MAX_STABILIZATION_TICKS {
            let (status, visited) = self.tick_once()?;
            self.cancel_unvisited(&visited);

            if matches!(status, Success | Failure) {
                return Ok(status);
            }

            if previous_visited.as_ref() == Some(&visited) {
                return Ok(Running);
            }
            previous_visited = Some(visited);
        }

        Err(RunError::StabilizationLimit)
    }

    fn tick_once(&mut self) -> Result<(Status, HashSet<ActionId>), RunError> {
        let mut visited = HashSet::new();
        let mut missing_action = None;
        let event = Event::zero_dt_args();

        let Self {
            tree,
            actions,
            executor,
            active,
            completed,
            tasks,
            ..
        } = self;

        let result = tree.tick(&event, &mut |args, _| {
            let id = args.action.clone();
            visited.insert(id.clone());

            if let Some(outcome) = completed.get(&id) {
                return match outcome {
                    ActionOutcome::Success => (Success, args.dt),
                    ActionOutcome::Failure => (Failure, args.dt),
                };
            }

            if active.contains_key(&id) {
                return (Running, args.dt);
            }

            let Some(action) = actions.remove(&id) else {
                missing_action = Some(id);
                return (Failure, args.dt);
            };

            let action_token = CancellationToken::new();
            active.insert(id.clone(), action_token.clone());

            let action_executor = Arc::clone(executor);
            let task_id = id;
            tasks.spawn(async move {
                let action_future =
                    action_executor.execute(task_id.clone(), action, action_token.clone());
                let exit = tokio::select! {
                    biased;
                    _ = action_token.cancelled() => TaskExit::Cancelled,
                    outcome = action_future => TaskExit::Completed(outcome),
                };
                (task_id, exit)
            });

            (Running, args.dt)
        });

        if let Some(id) = missing_action {
            return Err(RunError::MissingAction(id));
        }

        result
            .map(|(status, _remaining_dt)| (status, visited))
            .ok_or(RunError::UnexpectedFinishedTree)
    }

    fn cancel_unvisited(&mut self, visited: &HashSet<ActionId>) {
        for (id, token) in &self.active {
            if !visited.contains(id) {
                self.abandoned.insert(id.clone());
                token.cancel();
            }
        }
    }

    fn cancel_all_tokens(&mut self) {
        for (id, token) in &self.active {
            self.abandoned.insert(id.clone());
            token.cancel();
        }
    }

    fn record_task_exit(&mut self, (id, exit): (ActionId, TaskExit)) {
        self.active.remove(&id);

        match exit {
            TaskExit::Completed(outcome) if !self.abandoned.contains(&id) => {
                self.completed.insert(id, outcome);
            }
            TaskExit::Completed(_) | TaskExit::Cancelled => {
                self.abandoned.insert(id);
            }
        }
    }

    async fn cancel_and_drain_all(&mut self) -> Result<(), RunError> {
        self.cancel_all_tokens();
        let mut first_error = None;

        while let Some(task) = self.tasks.join_next().await {
            match task {
                Ok(exit) => self.record_task_exit(exit),
                Err(error) if first_error.is_none() => {
                    first_error = Some(error);
                }
                Err(_) => {}
            }
        }

        if let Some(error) = first_error {
            return Err(RunError::TaskJoin(error));
        }
        Ok(())
    }

    async fn cancel_and_drain_after_error(&mut self) {
        self.cancel_all_tokens();

        while let Some(task) = self.tasks.join_next().await {
            if let Ok(exit) = task {
                self.record_task_exit(exit);
            }
        }
    }
}

fn compile_node<A>(
    node: Node<A>,
    actions: &mut HashMap<ActionId, A>,
) -> Result<Behavior<ActionId>, ValidationError> {
    match node {
        Node::Action { id, action } => {
            if id.as_str().trim().is_empty() {
                return Err(ValidationError::EmptyActionId);
            }
            if actions.insert(id.clone(), action).is_some() {
                return Err(ValidationError::DuplicateActionId(id));
            }
            Ok(Action(id))
        }
        Node::Sequence(children) => {
            compile_composite(CompositeKind::Sequence, children, actions, Sequence)
        }
        Node::Fallback(children) => {
            compile_composite(CompositeKind::Fallback, children, actions, Select)
        }
        Node::ParallelAll(children) => {
            compile_composite(CompositeKind::ParallelAll, children, actions, WhenAll)
        }
        Node::Race(children) => compile_composite(CompositeKind::Race, children, actions, Race),
    }
}

fn compile_composite<A>(
    kind: CompositeKind,
    children: Vec<Node<A>>,
    actions: &mut HashMap<ActionId, A>,
    constructor: fn(Vec<Behavior<ActionId>>) -> Behavior<ActionId>,
) -> Result<Behavior<ActionId>, ValidationError> {
    if children.is_empty() {
        return Err(ValidationError::EmptyComposite(kind));
    }

    children
        .into_iter()
        .map(|child| compile_node(child, actions))
        .collect::<Result<Vec<_>, _>>()
        .map(constructor)
}
