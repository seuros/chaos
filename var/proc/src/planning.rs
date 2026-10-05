//! Durable planning contract. References are addresses, never display positions.
pub use chaos_ipc::plan_tool::{PlanStatus, TaskStatus};
use serde::{Deserialize, Serialize};
use state_machines::state_machine;

mod content;
pub use content::*;
mod consolidation;
pub use consolidation::*;

/// Capabilities of the mounted storage, shared by prompt and tool rendering.
/// No storage is the default: previews must not advertise unavailable tools.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanningCapabilities {
    pub available: bool,
    pub dependencies: bool,
    pub shared: bool,
}

impl PlanningCapabilities {
    pub const SQLITE: Self = Self {
        available: true,
        dependencies: false,
        shared: false,
    };
    pub const POSTGRES: Self = Self {
        available: true,
        dependencies: true,
        shared: true,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskEvent {
    Start,
    Block,
    Unblock,
    Complete,
    Cancel,
    Reopen,
}

#[derive(Debug, Clone, Copy)]
pub enum PlanEvent {
    Complete,
    Cancel,
    Reopen,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Workspace {
    pub id: String,
    pub code: String,
    pub name: String,
    pub revision: i16,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Project {
    pub id: String,
    pub workspace_id: String,
    pub name: String,
    pub revision: i16,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Checkout {
    #[sqlx(try_from = "String")]
    pub path: std::path::PathBuf,
    pub project_id: String,
    pub workspace_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
pub struct Plan {
    pub id: String,
    pub workspace_id: String,
    pub reference: String,
    pub title: String,
    #[sqlx(try_from = "String")]
    pub status: PlanStatus,
    pub revision: i16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
pub struct Task {
    pub id: String,
    pub reference: String,
    pub title: String,
    #[sqlx(try_from = "String")]
    pub status: TaskStatus,
    pub revision: i16,
    pub parent_id: Option<String>,
    pub position: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanSnapshot {
    pub capabilities: PlanningCapabilities,
    pub plan: Plan,
    pub tasks: Vec<TaskNode>,
    pub next_offset: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
pub struct TaskNode {
    #[serde(flatten)]
    #[sqlx(flatten)]
    pub task: Task,
    pub depth: i64,
}

impl From<PlanSnapshot> for chaos_ipc::plan_tool::PlanUpdate {
    fn from(snapshot: PlanSnapshot) -> Self {
        Self {
            plan_id: snapshot.plan.id,
            reference: snapshot.plan.reference,
            title: snapshot.plan.title,
            status: snapshot.plan.status,
            revision: snapshot.plan.revision,
            tasks: snapshot
                .tasks
                .into_iter()
                .map(|node| chaos_ipc::plan_tool::PlanTask {
                    reference: node.task.reference,
                    title: node.task.title,
                    status: node.task.status,
                    depth: node.depth,
                })
                .collect(),
            next_offset: snapshot.next_offset,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskDetail {
    pub task: Task,
    #[serde(flatten)]
    pub content: PlanningContent,
    pub projects: Page<Project>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MutationResult {
    pub plan: Plan,
    pub task: Option<Task>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next_offset: Option<i64>,
}

impl<T> Page<T> {
    pub(crate) fn new(mut items: Vec<T>, offset: i64) -> Self {
        let next_offset = (items.len() > 50).then_some(offset + 50);
        items.truncate(50);
        Self { items, next_offset }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, sqlx::FromRow)]
pub struct HistoryEntry {
    pub id: String,
    pub seq: i64,
    pub task_id: Option<String>,
    pub actor: String,
    pub installation_id: String,
    #[sqlx(json)]
    pub event: PlanChange,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HistoryPage {
    pub entries: Vec<HistoryEntry>,
    pub next_after: Option<i64>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Dependency {
    pub parent_id: String,
    pub child_id: String,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct ClosureMismatch {
    pub ancestor_id: String,
    pub descendant_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum PlanChange {
    Create {
        workspace: String,
        title: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        body: String,
    },
    Edit {
        title: String,
    },
    AddTask {
        title: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        body: String,
        parent: Option<String>,
        position: i64,
    },
    EditTask {
        task: String,
        task_revision: i16,
        title: String,
        projects: Vec<String>,
    },
    MoveTask {
        task: String,
        task_revision: i16,
        parent: Option<String>,
        position: i64,
    },
    Transition {
        task: String,
        task_revision: i16,
        event: TaskEvent,
        reason: String,
    },
    Note {
        task: Option<String>,
        note: String,
    },
    Clarify {
        task: Option<String>,
        text: String,
    },
    Link {
        parent: String,
        child: String,
    },
    Unlink {
        parent: String,
        child: String,
    },
    Complete {
        reason: String,
    },
    Cancel {
        reason: String,
    },
    Reopen {
        reason: String,
    },
}

impl PlanChange {
    pub fn progress_only(&self) -> bool {
        matches!(
            self,
            Self::Transition { .. }
                | Self::Note { .. }
                | Self::Complete { .. }
                | Self::Cancel { .. }
        )
    }
    pub fn structural(&self) -> bool {
        matches!(
            self,
            Self::Edit { .. }
                | Self::AddTask { .. }
                | Self::MoveTask { .. }
                | Self::Link { .. }
                | Self::Unlink { .. }
                | Self::Complete { .. }
                | Self::Cancel { .. }
                | Self::Reopen { .. }
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanMutation {
    pub request_id: String,
    pub plan: Option<String>,
    pub expected_revision: Option<i16>,
    #[serde(flatten)]
    pub change: PlanChange,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlanningActor {
    pub session: String,
    pub installation: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RevisionConflict {
    pub error: &'static str,
    pub id: String,
    pub expected_revision: Option<i16>,
    pub current_revision: i16,
}

impl std::fmt::Display for RevisionConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            serde_json::to_string(self).map_err(|_| std::fmt::Error)?
        )
    }
}
impl std::error::Error for RevisionConflict {}

pub(crate) fn check_revision(id: &str, expected: Option<i16>, current: i16) -> anyhow::Result<()> {
    if expected != Some(current) {
        return Err(RevisionConflict {
            error: "revision_conflict",
            id: id.into(),
            expected_revision: expected,
            current_revision: current,
        }
        .into());
    }
    Ok(())
}

state_machine! {
    name: PlanningTaskLifecycle,
    dynamic: true,
    initial: Pending,
    states: [Pending, InProgress, Blocked, Completed, Cancelled],
    events {
        start { transition: { from: Pending, to: InProgress } }
        block {
            transition: { from: Pending, to: Blocked }
            transition: { from: InProgress, to: Blocked }
        }
        unblock { transition: { from: Blocked, to: Pending } }
        complete { transition: { from: InProgress, to: Completed } }
        cancel {
            transition: { from: Pending, to: Cancelled }
            transition: { from: InProgress, to: Cancelled }
            transition: { from: Blocked, to: Cancelled }
        }
        reopen {
            transition: { from: Completed, to: Pending }
            transition: { from: Cancelled, to: Pending }
        }
    }
}

pub(crate) fn transition(status: TaskStatus, event: TaskEvent) -> anyhow::Result<TaskStatus> {
    use PlanningTaskLifecycleEvent as E;
    use PlanningTaskLifecycleState as S;
    let state = match status {
        TaskStatus::Pending => S::Pending,
        TaskStatus::InProgress => S::InProgress,
        TaskStatus::Blocked => S::Blocked,
        TaskStatus::Completed => S::Completed,
        TaskStatus::Cancelled => S::Cancelled,
    };
    let event = match event {
        TaskEvent::Start => E::Start,
        TaskEvent::Block => E::Block,
        TaskEvent::Unblock => E::Unblock,
        TaskEvent::Complete => E::Complete,
        TaskEvent::Cancel => E::Cancel,
        TaskEvent::Reopen => E::Reopen,
    };
    let mut machine = DynamicPlanningTaskLifecycle::new_init_state((), state);
    anyhow::ensure!(machine.handle(event).is_ok(), "invalid task transition");
    Ok(match machine.current_state() {
        S::Pending => TaskStatus::Pending,
        S::InProgress => TaskStatus::InProgress,
        S::Blocked => TaskStatus::Blocked,
        S::Completed => TaskStatus::Completed,
        S::Cancelled => TaskStatus::Cancelled,
    })
}

mod plan_lifecycle {
    use super::*;
    state_machine! {
        name: PlanningPlanLifecycle,
        dynamic: true,
        initial: Active,
        states: [Active, Completed, Cancelled],
        events {
            complete { transition: { from: Active, to: Completed } }
            cancel { transition: { from: Active, to: Cancelled } }
            reopen {
                transition: { from: Completed, to: Active }
                transition: { from: Cancelled, to: Active }
            }
        }
    }

    pub(crate) fn plan_transition(
        status: PlanStatus,
        event: PlanEvent,
    ) -> anyhow::Result<PlanStatus> {
        use PlanningPlanLifecycleEvent as E;
        use PlanningPlanLifecycleState as S;
        let state = match status {
            PlanStatus::Active => S::Active,
            PlanStatus::Completed => S::Completed,
            PlanStatus::Cancelled => S::Cancelled,
        };
        let event = match event {
            PlanEvent::Complete => E::Complete,
            PlanEvent::Cancel => E::Cancel,
            PlanEvent::Reopen => E::Reopen,
        };
        let mut machine = DynamicPlanningPlanLifecycle::new_init_state((), state);
        anyhow::ensure!(machine.handle(event).is_ok(), "invalid plan transition");
        Ok(match machine.current_state() {
            S::Active => PlanStatus::Active,
            S::Completed => PlanStatus::Completed,
            S::Cancelled => PlanStatus::Cancelled,
        })
    }
}
pub(crate) use plan_lifecycle::plan_transition;
