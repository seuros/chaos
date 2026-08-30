use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Stable identifier used to correlate a declarative leaf with its runtime
/// action.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ActionId(String);

impl ActionId {
    /// Creates an action identifier.
    ///
    /// Empty or whitespace-only values are rejected when a [`Synopsis`] is
    /// passed to [`crate::Runner::new`].
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the identifier as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for ActionId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for ActionId {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl fmt::Display for ActionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// A node in a declarative ChaOS synopsis.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Node<A> {
    /// Launches one asynchronous action.
    Action {
        /// Stable, synopsis-wide unique identifier.
        id: ActionId,
        /// Backend-specific action description.
        action: A,
    },
    /// Runs children in order and fails when any child fails.
    Sequence(Vec<Self>),
    /// Runs children in order until one succeeds.
    Fallback(Vec<Self>),
    /// Runs every child concurrently and succeeds when all children succeed.
    ParallelAll(Vec<Self>),
    /// Runs every child concurrently and returns the first terminal result.
    Race(Vec<Self>),
}

impl<A> Node<A> {
    /// Creates an action leaf.
    pub fn action(id: impl Into<ActionId>, action: A) -> Self {
        Self::Action {
            id: id.into(),
            action,
        }
    }

    /// Creates a sequential composite.
    pub fn sequence(children: impl IntoIterator<Item = Self>) -> Self {
        Self::Sequence(children.into_iter().collect())
    }

    /// Creates a fallback composite.
    pub fn fallback(children: impl IntoIterator<Item = Self>) -> Self {
        Self::Fallback(children.into_iter().collect())
    }

    /// Creates a parallel-all composite.
    pub fn parallel_all(children: impl IntoIterator<Item = Self>) -> Self {
        Self::ParallelAll(children.into_iter().collect())
    }

    /// Creates a race composite.
    pub fn race(children: impl IntoIterator<Item = Self>) -> Self {
        Self::Race(children.into_iter().collect())
    }
}

/// Serializable, runtime-independent workflow definition.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Synopsis<A> {
    root: Node<A>,
}

impl<A> Synopsis<A> {
    /// Creates a synopsis rooted at `root`.
    pub fn new(root: Node<A>) -> Self {
        Self { root }
    }

    /// Returns the root node.
    pub fn root(&self) -> &Node<A> {
        &self.root
    }

    /// Consumes the synopsis and returns its root node.
    pub fn into_root(self) -> Node<A> {
        self.root
    }
}

/// Terminal result returned by an action executor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActionOutcome {
    /// The action completed successfully.
    Success,
    /// The action completed unsuccessfully.
    Failure,
}

/// Terminal result returned by a synopsis runner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The root node completed successfully.
    Success,
    /// The root node completed unsuccessfully.
    Failure,
    /// External cancellation stopped the synopsis.
    Cancelled,
}

/// Composite category included in validation errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompositeKind {
    /// A sequential composite.
    Sequence,
    /// A fallback composite.
    Fallback,
    /// A parallel-all composite.
    ParallelAll,
    /// A race composite.
    Race,
}

impl fmt::Display for CompositeKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sequence => formatter.write_str("sequence"),
            Self::Fallback => formatter.write_str("fallback"),
            Self::ParallelAll => formatter.write_str("parallel-all"),
            Self::Race => formatter.write_str("race"),
        }
    }
}

/// Error returned when a synopsis cannot be compiled safely.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ValidationError {
    /// An action identifier was empty or contained only whitespace.
    #[error("action IDs cannot be empty")]
    EmptyActionId,
    /// Multiple leaves used the same identifier.
    #[error("duplicate action ID `{0}`")]
    DuplicateActionId(ActionId),
    /// A composite contained no children.
    #[error("{0} composites cannot be empty")]
    EmptyComposite(CompositeKind),
}
