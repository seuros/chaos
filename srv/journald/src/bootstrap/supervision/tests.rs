use std::time::Duration;

use super::BASE_DELAY;
use super::HISTORY_FILENAME;
use super::Verdict;
use super::decide;
use super::read_history;
use super::write_history;

const SECOND: u64 = 1_000_000_000;

#[test]
fn sixth_restart_within_the_window_is_refused() {
    let mut history = Vec::new();
    for n in 0..5 {
        let decision = decide(history.clone(), n * SECOND);
        assert!(
            matches!(decision.verdict, Verdict::Spawn { .. }),
            "restart {n}"
        );
        history = decision.history;
    }
    assert_eq!(history.len(), 5);

    let refused = decide(history.clone(), 5 * SECOND);
    assert_eq!(refused.verdict, Verdict::Refuse { recent: 5 });
    assert_eq!(refused.history, history);
}

#[test]
fn history_from_another_process_counts() {
    let foreign: Vec<u64> = (1..=5).map(|n| n * SECOND).collect();
    let decision = decide(foreign, 10 * SECOND);
    assert_eq!(decision.verdict, Verdict::Refuse { recent: 5 });
}

#[test]
fn stale_history_outside_the_window_is_dropped() {
    let stale: Vec<u64> = (1..=5).map(|n| n * SECOND).collect();
    let now = 200 * SECOND;
    let decision = decide(stale, now);
    assert_eq!(
        decision.verdict,
        Verdict::Spawn {
            delay: Duration::ZERO
        }
    );
    assert_eq!(decision.history, vec![now]);
}

#[test]
fn repeated_restarts_back_off_exponentially() {
    let first = decide(Vec::new(), 0);
    assert_eq!(
        first.verdict,
        Verdict::Spawn {
            delay: Duration::ZERO
        }
    );
    let second = decide(first.history, SECOND);
    assert_eq!(
        second.verdict,
        Verdict::Spawn {
            delay: BASE_DELAY * 2
        }
    );
    let third = decide(second.history, 2 * SECOND);
    assert_eq!(
        third.verdict,
        Verdict::Spawn {
            delay: BASE_DELAY * 4
        }
    );
}

#[test]
fn history_file_round_trips_and_tolerates_absence() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let path = dir.path().join(HISTORY_FILENAME);
    assert_eq!(
        read_history(&path).expect("missing file reads empty"),
        Vec::<u64>::new()
    );

    let history = vec![SECOND, 2 * SECOND, 3 * SECOND];
    write_history(&path, &history).expect("write history");
    assert_eq!(read_history(&path).expect("read history"), history);
    assert_eq!(
        std::fs::read_to_string(&path).expect("raw file"),
        "1000000000\n2000000000\n3000000000\n"
    );
}
