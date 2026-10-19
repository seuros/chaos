use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use chaos_synopsis::{
    ActionExecutor, ActionFuture, ActionId, ActionOutcome, CompositeKind, Node, Outcome, RunError,
    Runner, Synopsis, ValidationError,
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
    tokens: Arc<Mutex<HashMap<ActionId, CancellationToken>>>,
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

    fn remember_token(&self, id: &ActionId, cancellation: CancellationToken) {
        self.tokens
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id.clone(), cancellation);
    }

    fn assert_cancelled(&self, expected: &str) {
        let tokens = self.tokens.lock().unwrap_or_else(PoisonError::into_inner);
        assert!(
            tokens
                .get(&id(expected))
                .is_some_and(CancellationToken::is_cancelled),
            "{expected} must receive token cancellation"
        );
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
        cancellation: CancellationToken,
    ) -> ActionFuture {
        self.remember_token(&id, cancellation);
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

    std::assert_matches!(outcome, Ok(Outcome::Success));
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

    std::assert_matches!(outcome, Ok(Outcome::Success));
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

    std::assert_matches!(outcome, Ok(Outcome::Success));
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

    std::assert_matches!(outcome, Ok(Outcome::Failure));
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

    std::assert_matches!(outcome, Ok(Outcome::Success));
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

    std::assert_matches!(outcome, Ok(Outcome::Success));
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

    std::assert_matches!(outcome, Ok(Outcome::Success));
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
    executor.assert_cancelled("race-loser");
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
    std::assert_matches!(outcome, Ok(Outcome::Cancelled));

    let events = executor.events();
    assert!(events.contains(&MockEvent::Cancelled(id("left"))));
    assert!(events.contains(&MockEvent::Cancelled(id("right"))));
    executor.assert_cancelled("left");
    executor.assert_cancelled("right");
}

#[tokio::test(start_paused = true)]
async fn race_failure_is_terminal_without_waiting_for_success() {
    let executor = MockExecutor::default();
    let synopsis = Node::race([
        action("failure", MockAction::fails_after(10)),
        action("slow-success", MockAction::succeeds_after(1_000)),
    ]);
    let start = tokio::time::Instant::now();

    let outcome = runner(synopsis, &executor)
        .run(CancellationToken::new())
        .await;

    std::assert_matches!(outcome, Ok(Outcome::Failure));
    assert_eq!(start.elapsed(), Duration::from_millis(10));
    executor.assert_cancelled("slow-success");
    assert!(
        executor
            .events()
            .contains(&MockEvent::Cancelled(id("slow-success")))
    );
}

#[tokio::test(start_paused = true)]
async fn simultaneous_race_completions_use_declared_order_without_relaunching() {
    for first in [ActionOutcome::Success, ActionOutcome::Failure] {
        let executor = MockExecutor::default();
        let second = match first {
            ActionOutcome::Success => ActionOutcome::Failure,
            ActionOutcome::Failure => ActionOutcome::Success,
        };
        let synopsis = Node::race([
            action(
                "left",
                MockAction {
                    delay_ms: 10,
                    outcome: first,
                },
            ),
            action(
                "right",
                MockAction {
                    delay_ms: 10,
                    outcome: second,
                },
            ),
        ]);
        let outcome = runner(synopsis, &executor)
            .run(CancellationToken::new())
            .await;
        let expected = match first {
            ActionOutcome::Success => Outcome::Success,
            ActionOutcome::Failure => Outcome::Failure,
        };
        std::assert_matches!(outcome, Ok(actual) if actual == expected);
        assert_eq!(executor.started_count("left"), 1);
        assert_eq!(executor.started_count("right"), 1);
    }
}

#[tokio::test(start_paused = true)]
async fn dropping_run_future_cancels_tokens_and_drops_all_action_futures() {
    let executor = MockExecutor::default();
    let synopsis = Node::parallel_all([
        action("left", MockAction::succeeds_after(60_000)),
        action("right", MockAction::succeeds_after(60_000)),
    ]);
    let mut run = Box::pin(runner(synopsis, &executor).run(CancellationToken::new()));
    assert!(futures::poll!(run.as_mut()).is_pending());
    assert_eq!(executor.started_count("left"), 1);
    assert_eq!(executor.started_count("right"), 1);

    drop(run);

    executor.assert_cancelled("left");
    executor.assert_cancelled("right");
    let events = executor.events();
    assert!(events.contains(&MockEvent::Cancelled(id("left"))));
    assert!(events.contains(&MockEvent::Cancelled(id("right"))));
}

#[tokio::test(start_paused = true)]
async fn pre_cancelled_run_does_not_start_actions() {
    let executor = MockExecutor::default();
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let outcome = runner(action("never", MockAction::succeeds_after(1)), &executor)
        .run(cancellation)
        .await;

    std::assert_matches!(outcome, Ok(Outcome::Cancelled));
    assert!(executor.events().is_empty());
}

struct CheckHaltExecutor(MockExecutor);

impl ActionExecutor<MockAction> for CheckHaltExecutor {
    fn execute(
        &self,
        id: ActionId,
        action: MockAction,
        cancellation: CancellationToken,
    ) -> ActionFuture {
        if id.as_str() == "after-race" {
            self.0.assert_cancelled("loser-left");
            self.0.assert_cancelled("loser-right");
        }
        self.0.execute(id, action, cancellation)
    }
}

#[tokio::test(start_paused = true)]
async fn nested_composite_losers_receive_halt_before_following_action_starts() {
    let executor = MockExecutor::default();
    let synopsis = Node::sequence([
        Node::race([
            action("winner", MockAction::succeeds_after(10)),
            Node::parallel_all([
                action("loser-left", MockAction::succeeds_after(1_000)),
                Node::sequence([
                    action("loser-right", MockAction::succeeds_after(1_000)),
                    action("never", MockAction::succeeds_after(1)),
                ]),
            ]),
        ]),
        action("after-race", MockAction::succeeds_after(10)),
    ]);
    let built = Runner::new(
        Synopsis::new(synopsis),
        Arc::new(CheckHaltExecutor(executor.clone())),
    );
    let Ok(runner) = built else {
        panic!("nested test synopsis should validate");
    };

    std::assert_matches!(
        runner.run(CancellationToken::new()).await,
        Ok(Outcome::Success)
    );
    assert_eq!(executor.started_count("never"), 0);
    for name in ["loser-left", "loser-right"] {
        assert!(executor.events().contains(&MockEvent::Cancelled(id(name))));
    }
}

#[derive(Clone, Copy)]
enum PanicWhen {
    Creating,
    Polling,
}

struct PanicExecutor {
    inner: MockExecutor,
    when: PanicWhen,
}

impl ActionExecutor<MockAction> for PanicExecutor {
    fn execute(
        &self,
        id: ActionId,
        action: MockAction,
        cancellation: CancellationToken,
    ) -> ActionFuture {
        if id.as_str() != "panic" {
            return self.inner.execute(id, action, cancellation);
        }
        self.inner.remember_token(&id, cancellation);
        self.inner
            .lock_events()
            .push(MockEvent::Started(id.clone()));
        match self.when {
            PanicWhen::Creating => panic!("executor creation panic"),
            PanicWhen::Polling => {
                let guard = CancelOnDrop {
                    id,
                    events: Arc::clone(&self.inner.events),
                    completed: false,
                };
                Box::pin(async move {
                    let _guard = guard;
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    panic!("executor polling panic");
                })
            }
        }
    }
}

#[tokio::test(start_paused = true)]
async fn executor_panics_are_errors_not_fallback_results_and_cleanup_siblings() {
    for when in [PanicWhen::Creating, PanicWhen::Polling] {
        let executor = MockExecutor::default();
        let synopsis = Node::fallback([
            Node::parallel_all([
                action("panic", MockAction::succeeds_after(1)),
                action("sibling", MockAction::succeeds_after(60_000)),
            ]),
            action("must-not-start", MockAction::succeeds_after(1)),
        ]);
        let built = Runner::new(
            Synopsis::new(synopsis),
            Arc::new(PanicExecutor {
                inner: executor.clone(),
                when,
            }),
        );
        let Ok(runner) = built else {
            panic!("panic test synopsis should validate");
        };
        let outcome = runner.run(CancellationToken::new()).await;

        std::assert_matches!(
            outcome,
            Err(RunError::ActionPanicked { id: failed, message })
                if failed == id("panic") && message.contains("executor")
        );
        assert_eq!(executor.started_count("must-not-start"), 0);
        executor.assert_cancelled("panic");
        if executor.started_count("sibling") > 0 {
            executor.assert_cancelled("sibling");
            assert!(
                executor
                    .events()
                    .contains(&MockEvent::Cancelled(id("sibling")))
            );
        }
    }
}

#[tokio::test(start_paused = true)]
async fn simultaneous_race_success_does_not_hide_a_panicking_job() {
    let executor = MockExecutor::default();
    let synopsis = Node::race([
        action("success", MockAction::succeeds_after(10)),
        action("panic", MockAction::succeeds_after(10)),
    ]);
    let built = Runner::new(
        Synopsis::new(synopsis),
        Arc::new(PanicExecutor {
            inner: executor.clone(),
            when: PanicWhen::Polling,
        }),
    );
    let Ok(runner) = built else {
        panic!("race panic test synopsis should validate");
    };

    std::assert_matches!(
        runner.run(CancellationToken::new()).await,
        Err(RunError::ActionPanicked { id: failed, .. }) if failed == id("panic")
    );
    assert_eq!(executor.started_count("success"), 1);
    assert_eq!(executor.started_count("panic"), 1);
    executor.assert_cancelled("panic");
}

#[test]
fn rejects_empty_composites_duplicate_ids_and_blank_ids() {
    let executor: Arc<dyn ActionExecutor<MockAction>> = Arc::new(MockExecutor::default());
    let empty = Runner::new(
        Synopsis::new(Node::Sequence(Vec::new())),
        Arc::clone(&executor),
    );
    std::assert_matches!(
        empty.err(),
        Some(ValidationError::EmptyComposite(CompositeKind::Sequence))
    );

    let duplicate = Runner::new(
        Synopsis::new(Node::parallel_all([
            action("same", MockAction::succeeds_after(1)),
            action("same", MockAction::succeeds_after(1)),
        ])),
        Arc::clone(&executor),
    );
    std::assert_matches!(
        duplicate.err(),
        Some(ValidationError::DuplicateActionId(action_id))
            if action_id == id("same")
    );

    let blank = Runner::new(
        Synopsis::new(action("  ", MockAction::succeeds_after(1))),
        executor,
    );
    std::assert_matches!(blank.err(), Some(ValidationError::EmptyActionId));
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
use std::collections::HashMap;
