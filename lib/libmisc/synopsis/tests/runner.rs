use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use chaos_synopsis::{
    ActionExecutor, ActionFuture, ActionId, ActionOutcome, CompositeKind, Node, Outcome, Runner,
    Synopsis, ValidationError,
};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct MockAction {
    delay_ms: u64,
    outcome: ActionOutcome,
}

impl MockAction {
    fn succeeds_after(delay_ms: u64) -> Self {
        Self {
            delay_ms,
            outcome: ActionOutcome::Success,
        }
    }

    fn fails_after(delay_ms: u64) -> Self {
        Self {
            delay_ms,
            outcome: ActionOutcome::Failure,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum MockEvent {
    Started(ActionId),
    Completed(ActionId, ActionOutcome),
    Cancelled(ActionId),
}

#[derive(Clone, Default)]
struct MockExecutor {
    events: Arc<Mutex<Vec<MockEvent>>>,
}

impl MockExecutor {
    fn lock_events(&self) -> MutexGuard<'_, Vec<MockEvent>> {
        self.events.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn events(&self) -> Vec<MockEvent> {
        self.lock_events().clone()
    }

    fn started_count(&self, expected: &str) -> usize {
        self.events()
            .iter()
            .filter(|event| matches!(event, MockEvent::Started(id) if id.as_str() == expected))
            .count()
    }
}

struct CancelOnDrop {
    id: ActionId,
    events: Arc<Mutex<Vec<MockEvent>>>,
    completed: bool,
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if !self.completed {
            self.events
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(MockEvent::Cancelled(self.id.clone()));
        }
    }
}

impl ActionExecutor<MockAction> for MockExecutor {
    fn execute(
        &self,
        id: ActionId,
        action: MockAction,
        _cancellation: CancellationToken,
    ) -> ActionFuture {
        let events = Arc::clone(&self.events);
        Box::pin(async move {
            events
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(MockEvent::Started(id.clone()));

            let mut cancel_guard = CancelOnDrop {
                id: id.clone(),
                events: Arc::clone(&events),
                completed: false,
            };

            tokio::time::sleep(Duration::from_millis(action.delay_ms)).await;
            cancel_guard.completed = true;
            events
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(MockEvent::Completed(id, action.outcome));
            action.outcome
        })
    }
}

fn id(value: &str) -> ActionId {
    ActionId::new(value)
}

fn action(name: &str, plan: MockAction) -> Node<MockAction> {
    Node::action(name, plan)
}

fn runner(root: Node<MockAction>, executor: &MockExecutor) -> Runner<MockAction> {
    match Runner::new(Synopsis::new(root), Arc::new(executor.clone())) {
        Ok(runner) => runner,
        Err(error) => panic!("test synopsis should validate: {error}"),
    }
}

#[tokio::test(start_paused = true)]
async fn sequence_runs_actions_once_in_order() {
    let executor = MockExecutor::default();
    let synopsis = Node::sequence([
        action("prepare", MockAction::succeeds_after(20)),
        action("review", MockAction::succeeds_after(10)),
    ]);

    let outcome = runner(synopsis, &executor)
        .run(CancellationToken::new())
        .await;

    assert!(matches!(outcome, Ok(Outcome::Success)));
    assert_eq!(
        executor.events(),
        vec![
            MockEvent::Started(id("prepare")),
            MockEvent::Completed(id("prepare"), ActionOutcome::Success),
            MockEvent::Started(id("review")),
            MockEvent::Completed(id("review"), ActionOutcome::Success),
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn fallback_runs_after_failure() {
    let executor = MockExecutor::default();
    let synopsis = Node::fallback([
        action("primary", MockAction::fails_after(5)),
        action("backup", MockAction::succeeds_after(5)),
    ]);

    let outcome = runner(synopsis, &executor)
        .run(CancellationToken::new())
        .await;

    assert!(matches!(outcome, Ok(Outcome::Success)));
    assert_eq!(
        executor.events(),
        vec![
            MockEvent::Started(id("primary")),
            MockEvent::Completed(id("primary"), ActionOutcome::Failure),
            MockEvent::Started(id("backup")),
            MockEvent::Completed(id("backup"), ActionOutcome::Success),
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn parallel_all_runs_concurrently() {
    let executor = MockExecutor::default();
    let synopsis = Node::parallel_all([
        action("fast", MockAction::succeeds_after(20)),
        action("slow", MockAction::succeeds_after(50)),
    ]);
    let started_at = tokio::time::Instant::now();

    let outcome = runner(synopsis, &executor)
        .run(CancellationToken::new())
        .await;

    assert!(matches!(outcome, Ok(Outcome::Success)));
    assert_eq!(started_at.elapsed(), Duration::from_millis(50));

    let events = executor.events();
    let first_completion = events
        .iter()
        .position(|event| matches!(event, MockEvent::Completed(..)));
    let Some(first_completion) = first_completion else {
        panic!("at least one action should complete");
    };
    assert_eq!(
        events[..first_completion]
            .iter()
            .filter(|event| matches!(event, MockEvent::Started(_)))
            .count(),
        2
    );
}

#[tokio::test(start_paused = true)]
async fn parallel_failure_cancels_remaining_actions() {
    let executor = MockExecutor::default();
    let synopsis = Node::parallel_all([
        action("failure", MockAction::fails_after(10)),
        action("slow", MockAction::succeeds_after(1_000)),
    ]);

    let outcome = runner(synopsis, &executor)
        .run(CancellationToken::new())
        .await;

    assert!(matches!(outcome, Ok(Outcome::Failure)));
    let events = executor.events();
    assert!(events.contains(&MockEvent::Cancelled(id("slow"))));
}

#[tokio::test(start_paused = true)]
async fn race_cancels_slow_loser() {
    let executor = MockExecutor::default();
    let synopsis = Node::race([
        action("winner", MockAction::succeeds_after(10)),
        action("loser", MockAction::succeeds_after(100)),
    ]);

    let outcome = runner(synopsis, &executor)
        .run(CancellationToken::new())
        .await;

    assert!(matches!(outcome, Ok(Outcome::Success)));
    let events = executor.events();
    assert!(events.contains(&MockEvent::Cancelled(id("loser"))));
    assert!(!events.contains(&MockEvent::Completed(id("loser"), ActionOutcome::Success)));
}

#[tokio::test(start_paused = true)]
async fn running_action_is_not_launched_twice() {
    let executor = MockExecutor::default();
    let synopsis = action("single", MockAction::succeeds_after(100));

    let outcome = runner(synopsis, &executor)
        .run(CancellationToken::new())
        .await;

    assert!(matches!(outcome, Ok(Outcome::Success)));
    assert_eq!(executor.started_count("single"), 1);
}

#[tokio::test(start_paused = true)]
async fn abandoned_nested_race_branch_is_cancelled_before_next_action_finishes() {
    let executor = MockExecutor::default();
    let synopsis = Node::sequence([
        Node::race([
            action("race-winner", MockAction::succeeds_after(10)),
            action("race-loser", MockAction::succeeds_after(1_000)),
        ]),
        action("after-race", MockAction::succeeds_after(100)),
    ]);

    let outcome = runner(synopsis, &executor)
        .run(CancellationToken::new())
        .await;

    assert!(matches!(outcome, Ok(Outcome::Success)));
    let events = executor.events();
    let cancelled = events
        .iter()
        .position(|event| event == &MockEvent::Cancelled(id("race-loser")));
    let Some(cancelled) = cancelled else {
        panic!("losing branch should be cancelled");
    };
    let after_completed = events
        .iter()
        .position(|event| event == &MockEvent::Completed(id("after-race"), ActionOutcome::Success));
    let Some(after_completed) = after_completed else {
        panic!("next action should complete");
    };
    assert!(cancelled < after_completed);
}

#[tokio::test(start_paused = true)]
async fn external_cancellation_stops_all_actions() {
    let executor = MockExecutor::default();
    let synopsis = Node::parallel_all([
        action("left", MockAction::succeeds_after(1_000)),
        action("right", MockAction::succeeds_after(1_000)),
    ]);
    let cancellation = CancellationToken::new();
    let handle = tokio::spawn(runner(synopsis, &executor).run(cancellation.clone()));

    for _ in 0..10 {
        if executor.started_count("left") == 1 && executor.started_count("right") == 1 {
            break;
        }
        tokio::task::yield_now().await;
    }
    cancellation.cancel();

    let joined = handle.await;
    let Ok(outcome) = joined else {
        panic!("runner task should join");
    };
    assert!(matches!(outcome, Ok(Outcome::Cancelled)));

    let events = executor.events();
    assert!(events.contains(&MockEvent::Cancelled(id("left"))));
    assert!(events.contains(&MockEvent::Cancelled(id("right"))));
}

#[test]
fn rejects_empty_composites_duplicate_ids_and_blank_ids() {
    let executor: Arc<dyn ActionExecutor<MockAction>> = Arc::new(MockExecutor::default());
    let empty = Runner::new(
        Synopsis::new(Node::Sequence(Vec::new())),
        Arc::clone(&executor),
    );
    assert!(matches!(
        empty,
        Err(ValidationError::EmptyComposite(CompositeKind::Sequence))
    ));

    let duplicate = Runner::new(
        Synopsis::new(Node::parallel_all([
            action("same", MockAction::succeeds_after(1)),
            action("same", MockAction::succeeds_after(1)),
        ])),
        Arc::clone(&executor),
    );
    assert!(matches!(
        duplicate,
        Err(ValidationError::DuplicateActionId(action_id))
            if action_id == id("same")
    ));

    let blank = Runner::new(
        Synopsis::new(action("  ", MockAction::succeeds_after(1))),
        executor,
    );
    assert!(matches!(blank, Err(ValidationError::EmptyActionId)));
}

#[test]
fn synopsis_definition_round_trips_through_json() {
    let synopsis = Synopsis::new(Node::sequence([
        action("prepare", MockAction::succeeds_after(10)),
        Node::fallback([
            action("primary", MockAction::fails_after(5)),
            action("backup", MockAction::succeeds_after(5)),
        ]),
    ]));

    let serialized = serde_json::to_string(&synopsis);
    let Ok(json) = serialized else {
        panic!("synopsis should serialize");
    };
    let restored: Result<Synopsis<MockAction>, _> = serde_json::from_str(&json);
    let Ok(restored) = restored else {
        panic!("synopsis should deserialize");
    };
    assert_eq!(synopsis, restored);
}
