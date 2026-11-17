# mcp-goals

The `chaos/goals` MCP extension, minus the opinions about who runs it: wire
types for goal checkpoints, verdicts and restores, plus the one lifecycle
graph (`Active`, `Checking`, `Complete`, `Paused`) both ends agree on.

The host journals and evaluates; the driver proposes. Both ask the same
machine whether a move is legal, so neither can drift into its own idea of
what "complete" means.

```rust
use mcp_goals::{GoalLifecycleEvent, Status};

assert_eq!(Status::Active.next(GoalLifecycleEvent::Check(true)), Some(Status::Checking));
assert!(!Status::Paused.accepts(GoalLifecycleEvent::Reopen(true)));
```

Payloads are the caller's verdict on the evidence: `false` vetoes the move.

Requires Rust 1.99 or newer. Apache-2.0.
