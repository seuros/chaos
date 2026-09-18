use super::*;
use pretty_assertions::assert_eq;
use std::path::PathBuf;

fn test_cwd() -> PathBuf {
    // These tests only need a stable absolute cwd; using temp_dir() avoids baking Unix- or
    // Windows-specific root semantics into the fixtures.
    std::env::temp_dir()
}

pub(crate) async fn streaming_suite() {
    super::chunking::tests::streaming_chunking_suite();
    super::controller::tests::controller_loose_vs_tight_with_commit_ticks_matches_full().await;
    drain_n_clamps_to_available_lines();
}

fn drain_n_clamps_to_available_lines() {
    let mut state = StreamState::new(None, &test_cwd());
    state.enqueue(vec![Line::from("one")]);

    let drained = state.drain_n(8);
    assert_eq!(drained, vec![Line::from("one")]);
    assert!(state.is_idle());
}
